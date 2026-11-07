//! Repository diffs for the Changes view.
//!
//! A diff scope compares working tree against HEAD (`workspace`), staged against HEAD
//! (`staged`), or two commits (`commit`). All answers come from the `git` CLI so no new
//! dependency is needed. File lists come from `git status --porcelain` merged with
//! `git diff --numstat`; a single file's patch is `git diff` for that path.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("git is not available: {error}"))?;
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

pub fn repository(cwd: &Path) -> Value {
    let root = git(cwd, &["rev-parse", "--show-toplevel"])
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|_| cwd.to_string_lossy().to_string());
    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|text| text.trim().to_string())
        .ok()
        .filter(|branch| !branch.is_empty() && branch != "HEAD");
    let head = git(cwd, &["rev-parse", "HEAD"])
        .map(|text| text.trim().to_string())
        .ok();
    let unborn = head.is_none();
    json!({
        "root": PathBuf::from(&root).to_string_lossy(),
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

fn status_map(cwd: &Path, scope: &str) -> Vec<(String, String, Option<String>)> {
    let porcelain = match scope {
        "staged" => git(cwd, &["diff", "--cached", "--name-status", "-z"]).unwrap_or_default(),
        "commit" => String::new(),
        _ => git(cwd, &["diff", "--name-status", "-z"]).unwrap_or_default(),
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
        let status = match code {
            "A" => "added",
            "D" => "deleted",
            "M" => "modified",
            "R" => "renamed",
            "C" => "copied",
            "T" => "type_changed",
            "U" | "AA" | "DD" => "unmerged",
            _ => "modified",
        };
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

pub fn files(
    cwd: &Path,
    scope: &str,
    base: Option<&str>,
    head: Option<&str>,
    max_files: Option<usize>,
) -> Value {
    let repository = repository(cwd);
    let counts = numstat(cwd, scope, base, head);
    let count_by_path: std::collections::HashMap<String, (Option<u32>, Option<u32>, bool)> = counts
        .into_iter()
        .map(|(path, additions, deletions, binary)| (path, (additions, deletions, binary)))
        .collect();
    let mut files: Vec<Value> = status_map(cwd, scope)
        .into_iter()
        .map(|(path, status, old_path)| {
            let (additions, deletions, binary) = count_by_path
                .get(&path)
                .cloned()
                .unwrap_or((None, None, false));
            json!({
                "path": path,
                "old_path": old_path,
                "status": status,
                "additions": additions,
                "deletions": deletions,
                "binary": if binary { json!(true) } else { Value::Null },
            })
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
                if !files.iter().any(|file| file["path"] == json!(path)) {
                    files.push(json!({
                        "path": path,
                        "old_path": null,
                        "status": "untracked",
                        "additions": null,
                        "deletions": null,
                        "binary": null,
                    }));
                }
            }
        }
    }
    files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    let total = files.len();
    let max = max_files.unwrap_or(2000);
    let truncated = files.len() > max;
    files.truncate(max);
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
    let mut file = json!({
        "path": path,
        "old_path": old_path,
        "status": "modified",
        "additions": null,
        "deletions": null,
        "binary": null,
    });
    let args: Vec<String> = match scope {
        "staged" => vec!["diff".into(), "--cached".into(), "--".into(), path.into()],
        "commit" => match (base, head) {
            (Some(base), Some(head)) => vec![
                "diff".into(),
                base.into(),
                head.into(),
                "--".into(),
                path.into(),
            ],
            _ => vec!["diff".into(), "--".into(), path.into()],
        },
        _ => vec!["diff".into(), "HEAD".into(), "--".into(), path.into()],
    };
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    let patch = git(cwd, &borrowed).unwrap_or_default();
    let unavailable = if patch.is_empty() {
        if is_binary_at(cwd, path) {
            "binary"
        } else {
            "unsupported"
        }
    } else if patch.contains("Binary files ") {
        "binary"
    } else {
        ""
    };
    if unavailable == "binary" {
        file["binary"] = json!(true);
    }
    json!({
        "repository": repository,
        "scope": scope,
        "base": base,
        "head": head,
        "file": file,
        "patch": if patch.is_empty() { Value::Null } else { Value::String(patch) },
        "unavailable": if unavailable.is_empty() { Value::Null } else { Value::String(unavailable.to_string()) },
        "limits": { "max_patch_bytes": 262144 },
    })
}

fn is_binary_at(cwd: &Path, path: &str) -> bool {
    git(cwd, &["diff", "HEAD", "--numstat", "--", path])
        .map(|text| text.contains("-\t-"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_and_files_report_a_dirty_tree() {
        let dir = std::env::temp_dir().join(format!("orion-diff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("repo dir");
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git runs");
            assert!(output.status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("a.txt"), "one\n").expect("seed");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "seed"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").expect("dirty");
        std::fs::write(dir.join("b.txt"), "new\n").expect("untracked");

        let repo = repository(&dir);
        assert_eq!(repo["unborn"], false);
        assert!(repo["branch"].is_string());

        let list = files(&dir, "workspace", None, None, None);
        let paths = list["files"]
            .as_array()
            .expect("files")
            .iter()
            .map(|file| file["path"].as_str().expect("path").to_string())
            .collect::<Vec<_>>();
        assert!(
            paths.contains(&"a.txt".to_string()),
            "modified listed: {paths:?}"
        );
        assert!(
            paths.contains(&"b.txt".to_string()),
            "untracked listed: {paths:?}"
        );

        let patch = file_patch(&dir, "workspace", None, None, "a.txt", None);
        let patch_text = patch["patch"].as_str().expect("patch text");
        assert!(
            patch_text.contains("+two"),
            "patch shows the addition: {patch_text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
