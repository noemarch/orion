//! Repository diffs for the Changes view.
//!
//! A diff scope compares working tree against HEAD (`workspace`), staged against HEAD
//! (`staged`), or two commits (`commit`). All answers come from the `git` CLI so no new
//! dependency is needed. File lists come from `git diff --name-status` merged with
//! `git diff --numstat`; a single file's patch is `git diff` for that path.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{json, Value};

/// The largest patch the file route hands over, in bytes. A bigger patch is
/// reported as unavailable instead of flooding the client.
const MAX_PATCH_BYTES: usize = 262_144;

fn git_output(cwd: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("git is not available: {error}"))
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_output(cwd, args)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            format!("git {} failed: {stderr}", args.join(" "))
        })
    }
}

/// `git diff` for one pair of paths. `git diff --no-index` exits with 1 when the
/// files differ, and that difference IS the answer, so exit code 1 is a success
/// here while every other failure stays an error.
fn git_diff(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_output(cwd, args)?;
    if output.status.success() || output.status.code() == Some(1) {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            format!("git {} failed: {stderr}", args.join(" "))
        })
    }
}

/// The working tree's top folder. Every path git prints is relative to it, so
/// the diffs are asked for from there rather than from an arbitrary subfolder.
fn repository_root(cwd: &Path) -> PathBuf {
    git(cwd, &["rev-parse", "--show-toplevel"])
        .map(|text| PathBuf::from(text.trim()))
        .unwrap_or_else(|_| cwd.to_path_buf())
}

pub fn repository(cwd: &Path) -> Value {
    let root = repository_root(cwd);
    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|text| text.trim().to_string())
        .ok()
        .filter(|branch| !branch.is_empty() && branch != "HEAD");
    let head = git(cwd, &["rev-parse", "HEAD"])
        .map(|text| text.trim().to_string())
        .ok();
    let unborn = head.is_none();
    json!({
        "root": root.to_string_lossy(),
        "branch": branch,
        "head": head,
        "detached": git(cwd, &["symbolic-ref", "-q", "HEAD"]).is_err(),
        "unborn": unborn,
    })
}

fn numstat(
    cwd: &Path,
    scope: &str,
    base: Option<&str>,
    head: Option<&str>,
) -> Vec<(String, Option<u32>, Option<u32>, bool)> {
    let text = match scope {
        "staged" => git(cwd, &["diff", "--cached", "--numstat"]).unwrap_or_default(),
        "commit" => match (base, head) {
            (Some(base), Some(head)) => {
                git(cwd, &["diff", "--numstat", base, head]).unwrap_or_default()
            }
            _ => String::new(),
        },
        _ => git(cwd, &["diff", "--numstat", "HEAD"]).unwrap_or_default(),
    };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let additions = parts.next()?;
            let deletions = parts.next()?;
            let path = parts.next()?.to_string();
            let parse = |value: &str| value.parse::<u32>().ok();
            Some((
                path,
                parse(additions),
                parse(deletions),
                additions == "-" || deletions == "-",
            ))
        })
        .collect()
}

/// The status of one path, as git's porcelain code names it. Git appends a
/// similarity score to a rename or a copy (`R100`, `C075`), so the code is read
/// from its first letter.
fn status_of(code: &str) -> &'static str {
    match code {
        "U" | "AA" | "DD" => "unmerged",
        _ => match code.as_bytes().first() {
            Some(b'A') => "added",
            Some(b'D') => "deleted",
            Some(b'M') => "modified",
            Some(b'R') => "renamed",
            Some(b'C') => "copied",
            Some(b'T') => "type_changed",
            _ => "modified",
        },
    }
}

fn status_map(
    cwd: &Path,
    scope: &str,
    base: Option<&str>,
    head: Option<&str>,
) -> Vec<(String, String, Option<String>)> {
    let porcelain = match scope {
        "staged" => git(cwd, &["diff", "--cached", "--name-status", "-z"]).unwrap_or_default(),
        "commit" => match (base, head) {
            (Some(base), Some(head)) => {
                git(cwd, &["diff", "--name-status", "-z", base, head]).unwrap_or_default()
            }
            _ => String::new(),
        },
        // The workspace scope compares the working tree with HEAD, so it asks
        // git for the same base the counts come from. Comparing with the index
        // instead would hide every change that was already staged.
        _ => git(cwd, &["diff", "HEAD", "--name-status", "-z"]).unwrap_or_default(),
    };
    let mut rows = Vec::new();
    let fields: Vec<&str> = porcelain.split('\0').collect();
    let mut index = 0;
    while index < fields.len() {
        let code = fields[index].trim();
        if code.is_empty() {
            index += 1;
            continue;
        }
        let status = status_of(code);
        if status == "renamed" || status == "copied" {
            let old = fields.get(index + 1).unwrap_or(&"").to_string();
            let new = fields.get(index + 2).unwrap_or(&"").to_string();
            rows.push((new, status.to_string(), Some(old)));
            index += 3;
        } else {
            let path = fields.get(index + 1).unwrap_or(&"").to_string();
            rows.push((path, status.to_string(), None));
            index += 2;
        }
    }
    rows
}

/// One changed file: how it changed, and how many lines it moved.
struct FileChange {
    path: String,
    old_path: Option<String>,
    status: String,
    additions: Option<u32>,
    deletions: Option<u32>,
    binary: bool,
}

/// Every file a comparison changed, with its counts. The file list and the
/// single-file patch both read this, so a file is described the same way
/// whichever route the reader asked.
fn changed_files(
    cwd: &Path,
    scope: &str,
    base: Option<&str>,
    head: Option<&str>,
) -> Vec<FileChange> {
    let counts = numstat(cwd, scope, base, head);
    let count_by_path: std::collections::HashMap<String, (Option<u32>, Option<u32>, bool)> = counts
        .into_iter()
        .map(|(path, additions, deletions, binary)| (path, (additions, deletions, binary)))
        .collect();
    let mut changes: Vec<FileChange> = status_map(cwd, scope, base, head)
        .into_iter()
        .map(|(path, status, old_path)| {
            let (additions, deletions, binary) = count_by_path
                .get(&path)
                .cloned()
                .unwrap_or((None, None, false));
            FileChange {
                path,
                old_path,
                status,
                additions,
                deletions,
                binary,
            }
        })
        .collect();
    // Untracked files show as untracked with no counts.
    if scope != "commit" {
        let stdout = git(
            cwd,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )
        .unwrap_or_default();
        for entry in stdout.split('\0') {
            if entry.len() > 3 && &entry[..2] == "??" {
                let path = entry[3..].to_string();
                if !changes.iter().any(|change| change.path == path) {
                    changes.push(FileChange {
                        path,
                        old_path: None,
                        status: "untracked".to_string(),
                        additions: None,
                        deletions: None,
                        binary: false,
                    });
                }
            }
        }
    }
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    changes
}

pub fn files(
    cwd: &Path,
    scope: &str,
    base: Option<&str>,
    head: Option<&str>,
    max_files: Option<usize>,
) -> Value {
    let repository = repository(cwd);
    let changes = changed_files(cwd, scope, base, head);
    let total = changes.len();
    let max = max_files.unwrap_or(2000);
    let truncated = total > max;
    let files: Vec<Value> = changes
        .into_iter()
        .take(max)
        .map(|change| {
            json!({
                "path": change.path,
                "old_path": change.old_path,
                "status": change.status,
                "additions": change.additions,
                "deletions": change.deletions,
                "binary": if change.binary { json!(true) } else { Value::Null },
            })
        })
        .collect();
    json!({
        "repository": repository,
        "scope": scope,
        "base": base,
        "head": head,
        "files": files,
        "total": total,
        "truncated": truncated,
        "limits": { "max_files": 2000, "max_list_bytes": 1048576 },
    })
}

pub fn file_patch(
    cwd: &Path,
    scope: &str,
    base: Option<&str>,
    head: Option<&str>,
    path: &str,
    old_path: Option<&str>,
) -> Value {
    let repository = repository(cwd);
    let root = repository_root(cwd);
    let changes = changed_files(cwd, scope, base, head);
    let change = changes.iter().find(|change| change.path == path);
    let status = change
        .map(|change| change.status.clone())
        .unwrap_or_else(|| "modified".to_string());
    // The rename's old path is known to the caller, and also to the file list;
    // either answer names the file's previous name.
    let old_path = old_path
        .map(String::from)
        .or_else(|| change.and_then(|change| change.old_path.clone()));
    let mut file = json!({
        "path": path,
        "old_path": old_path,
        "status": status,
        "additions": change.and_then(|change| change.additions),
        "deletions": change.and_then(|change| change.deletions),
        "binary": change.and_then(|change| if change.binary { Some(true) } else { None }),
    });

    // A commit comparison needs both ends. Without them there is nothing to
    // compare against, and quietly comparing the working tree with the index
    // would answer a question the reader never asked.
    if scope == "commit" && (base.is_none() || head.is_none()) {
        return json!({
            "repository": repository,
            "scope": scope,
            "base": base,
            "head": head,
            "file": file,
            "patch": Value::Null,
            "unavailable": "unsupported",
            "truncated": false,
            "limits": { "max_patch_bytes": MAX_PATCH_BYTES },
        });
    }

    // The pathspec carries both sides of a rename so git can pair them up; with
    // only the new path git would show the file as a fresh addition.
    let mut paths: Vec<&str> = Vec::new();
    if let Some(old) = old_path.as_deref() {
        if old != path {
            paths.push(old);
        }
    }
    paths.push(path);

    let patch = if status == "untracked" {
        // An untracked file has no diff against HEAD, so git is asked to
        // compare it with the empty file instead.
        git_diff(&root, &["diff", "--no-index", "--", "/dev/null", path]).unwrap_or_default()
    } else {
        let mut args: Vec<String> = match scope {
            "staged" => vec!["diff".into(), "--cached".into()],
            "commit" => vec![
                "diff".into(),
                base.unwrap_or_default().to_string(),
                head.unwrap_or_default().to_string(),
            ],
            _ => vec!["diff".into(), "HEAD".into()],
        };
        args.push("--".into());
        for path in &paths {
            args.push((*path).to_string());
        }
        let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
        git_diff(&root, &borrowed).unwrap_or_default()
    };

    let mut unavailable = Value::Null;
    let mut truncated = false;
    let mut patch_text = Value::Null;
    if status == "unmerged" {
        unavailable = json!("unmerged");
    } else if patch.contains("Binary files ") {
        unavailable = json!("binary");
        file["binary"] = json!(true);
    } else if patch.is_empty() {
        if is_binary_at(&root, scope, path) {
            unavailable = json!("binary");
            file["binary"] = json!(true);
        } else {
            unavailable = json!("unsupported");
        }
    } else if patch.len() > MAX_PATCH_BYTES {
        unavailable = json!("too_large");
        truncated = true;
    } else {
        patch_text = json!(patch);
    }

    json!({
        "repository": repository,
        "scope": scope,
        "base": base,
        "head": head,
        "file": file,
        "patch": patch_text,
        "unavailable": unavailable,
        "truncated": truncated,
        "limits": { "max_patch_bytes": MAX_PATCH_BYTES },
    })
}

fn is_binary_at(cwd: &Path, scope: &str, path: &str) -> bool {
    let args: Vec<&str> = match scope {
        "staged" => vec!["diff", "--cached", "--numstat", "--", path],
        _ => vec!["diff", "HEAD", "--numstat", "--", path],
    };
    git(cwd, &args)
        .map(|text| text.contains("-\t-"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway git repository with one committed file.
    struct TempRepo {
        path: PathBuf,
    }

    impl TempRepo {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("orion-diff-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("repo dir");
            let repo = TempRepo { path };
            repo.git(&["init", "-q"]);
            repo.git(&["config", "user.email", "test@example.com"]);
            repo.git(&["config", "user.name", "Test"]);
            std::fs::write(repo.path.join("a.txt"), "one\n").expect("seed");
            repo.git(&["add", "."]);
            repo.git(&["commit", "-q", "-m", "seed"]);
            repo
        }

        fn git(&self, args: &[&str]) {
            let output = Command::new("git")
                .args(args)
                .current_dir(&self.path)
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        fn write(&self, name: &str, contents: &str) {
            std::fs::write(self.path.join(name), contents).expect("write");
        }

        fn head(&self) -> String {
            git(&self.path, &["rev-parse", "HEAD"])
                .expect("head")
                .trim()
                .to_string()
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// The paths a file list holds, for a readable assertion.
    fn listed_paths(list: &Value) -> Vec<String> {
        list["files"]
            .as_array()
            .expect("files")
            .iter()
            .map(|file| file["path"].as_str().expect("path").to_string())
            .collect()
    }

    /// The row a file list holds for one path.
    fn listed_file<'a>(list: &'a Value, path: &str) -> &'a Value {
        list["files"]
            .as_array()
            .expect("files")
            .iter()
            .find(|file| file["path"] == json!(path))
            .unwrap_or_else(|| panic!("{path} is listed"))
    }

    #[test]
    fn repository_and_files_report_a_dirty_tree() {
        let repo = TempRepo::new();
        repo.write("a.txt", "one\ntwo\n");
        repo.write("b.txt", "new\n");

        let repository = repository(&repo.path);
        assert_eq!(repository["unborn"], false);
        assert!(repository["branch"].is_string());

        let list = files(&repo.path, "workspace", None, None, None);
        let paths = listed_paths(&list);
        assert!(
            paths.contains(&"a.txt".to_string()),
            "modified listed: {paths:?}"
        );
        assert!(
            paths.contains(&"b.txt".to_string()),
            "untracked listed: {paths:?}"
        );

        let patch = file_patch(&repo.path, "workspace", None, None, "a.txt", None);
        let patch_text = patch["patch"].as_str().expect("patch text");
        assert!(
            patch_text.contains("+two"),
            "patch shows the addition: {patch_text}"
        );
    }

    #[test]
    fn workspace_scope_lists_staged_only_changes() {
        let repo = TempRepo::new();
        repo.write("b.txt", "staged\n");
        repo.git(&["add", "b.txt"]);

        let list = files(&repo.path, "workspace", None, None, None);
        let staged = listed_file(&list, "b.txt");
        assert_eq!(staged["status"], json!("added"));
        assert_eq!(staged["additions"], json!(1));
        assert_eq!(staged["deletions"], json!(0));
    }

    #[test]
    fn commit_scope_lists_changed_files() {
        let repo = TempRepo::new();
        repo.write("a.txt", "one\ntwo\n");
        repo.git(&["commit", "-q", "-am", "second"]);
        let base = git(&repo.path, &["rev-parse", "HEAD~1"])
            .expect("base")
            .trim()
            .to_string();
        let head = repo.head();

        let list = files(&repo.path, "commit", Some(&base), Some(&head), None);
        let paths = listed_paths(&list);
        assert!(
            paths.contains(&"a.txt".to_string()),
            "the commit's change is listed: {paths:?}"
        );
    }

    #[test]
    fn untracked_file_patch_shows_the_new_file() {
        let repo = TempRepo::new();
        repo.write("new.txt", "brand new\n");

        let patch = file_patch(&repo.path, "workspace", None, None, "new.txt", None);
        let patch_text = patch["patch"].as_str().expect("patch text");
        assert!(
            patch_text.contains("new file mode"),
            "the patch says the file is new: {patch_text}"
        );
        assert!(
            patch_text.contains("+brand new"),
            "the patch shows the line: {patch_text}"
        );
        assert_eq!(patch["file"]["status"], json!("untracked"));
    }

    #[test]
    fn renamed_file_patch_shows_the_rename() {
        let repo = TempRepo::new();
        repo.git(&["mv", "a.txt", "b.txt"]);

        let list = files(&repo.path, "workspace", None, None, None);
        let renamed = listed_file(&list, "b.txt");
        assert_eq!(renamed["status"], json!("renamed"));
        assert_eq!(renamed["old_path"], json!("a.txt"));

        let patch = file_patch(&repo.path, "workspace", None, None, "b.txt", Some("a.txt"));
        let patch_text = patch["patch"].as_str().expect("patch text");
        assert!(
            patch_text.contains("rename from a.txt"),
            "the patch names the old path: {patch_text}"
        );
        assert!(
            patch_text.contains("rename to b.txt"),
            "the patch names the new path: {patch_text}"
        );
        assert_eq!(patch["file"]["status"], json!("renamed"));
    }

    #[test]
    fn patch_reports_the_files_real_status_and_counts() {
        let repo = TempRepo::new();
        repo.write("a.txt", "one\ntwo\nthree\n");

        let patch = file_patch(&repo.path, "workspace", None, None, "a.txt", None);
        assert_eq!(patch["file"]["status"], json!("modified"));
        assert_eq!(patch["file"]["additions"], json!(2));
        assert_eq!(patch["file"]["deletions"], json!(0));
        assert_eq!(patch["unavailable"], Value::Null);
    }

    #[test]
    fn an_oversized_patch_is_reported_as_too_large() {
        let repo = TempRepo::new();
        repo.write("big.txt", &"x".repeat(300_000));

        let patch = file_patch(&repo.path, "workspace", None, None, "big.txt", None);
        assert_eq!(patch["patch"], Value::Null);
        assert_eq!(patch["unavailable"], json!("too_large"));
        assert_eq!(patch["truncated"], json!(true));
    }
}
