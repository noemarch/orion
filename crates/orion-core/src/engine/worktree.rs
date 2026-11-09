//! Git worktrees for a workspace's repository.
//!
//! A worktree is a second checkout of the same repository on its own branch. Orion creates it
//! with `git worktree add`, lists it with `git worktree list --porcelain`, and removes it with
//! `git worktree remove` — always through the `git` CLI, so no new dependency. Opening a
//! worktree is just a workspace in its path, so agents run in it like anywhere else.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// Runs `git` in `cwd` and returns stdout. Stderr is captured for the error message.
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
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The repository's worktrees as `worktree list --porcelain` reports them.
pub fn list(cwd: &Path) -> Vec<Value> {
    let Ok(text) = git(cwd, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    parse_worktree_list(&text)
}

pub fn parse_worktree_list(text: &str) -> Vec<Value> {
    let mut worktrees = Vec::new();
    let mut current = serde_json::Map::new();
    for line in text.lines() {
        if line.is_empty() {
            if !current.is_empty() {
                worktrees.push(json!({
                    "path": current.remove("worktree").unwrap_or(Value::String(String::new())),
                    "branch": current.remove("branch").unwrap_or(Value::Null),
                    "head": current.remove("HEAD").unwrap_or(Value::Null),
                    "bare": current.remove("bare").is_some(),
                    "locked": current.remove("locked").unwrap_or(Value::Null),
                    "prunable": current.remove("prunable").unwrap_or(Value::Null),
                }));
                current.clear();
            }
            continue;
        }
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        current.insert(key.to_string(), Value::String(value.to_string()));
    }
    if !current.is_empty() {
        worktrees.push(json!({
            "path": current.remove("worktree").unwrap_or(Value::String(String::new())),
            "branch": current.remove("branch").unwrap_or(Value::Null),
            "head": current.remove("HEAD").unwrap_or(Value::Null),
            "bare": current.remove("bare").is_some(),
            "locked": current.remove("locked").unwrap_or(Value::Null),
            "prunable": current.remove("prunable").unwrap_or(Value::Null),
        }));
    }
    worktrees
}

/// Adds a worktree for `branch` at `path`, optionally from `base`.
pub fn create(cwd: &Path, path: &Path, branch: &str, base: Option<&str>) -> Result<(), String> {
    let mut args = vec![
        "worktree".to_string(),
        "add".to_string(),
        path.to_string_lossy().to_string(),
        "-b".to_string(),
        branch.to_string(),
    ];
    if let Some(base) = base {
        args.push(base.to_string());
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    git(cwd, &borrowed)?;
    Ok(())
}

/// Removes the worktree at `path`, with `--force` when asked.
/// Git refuses to remove the current worktree from inside it, so the command runs in the
/// main checkout instead.
pub fn remove(cwd: &Path, path: &Path, force: bool) -> Result<(), String> {
    let main = main_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut args = vec!["worktree".to_string(), "remove".to_string()];
    if force {
        args.push("--force".to_string());
    }
    args.push(path.to_string_lossy().to_string());
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    git(&main, &borrowed)?;
    Ok(())
}

fn main_root(cwd: &Path) -> Option<PathBuf> {
    let common = git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    let common = PathBuf::from(common.trim());
    common.parent().map(Path::to_path_buf)
}

pub fn default_worktree_path(cwd: &Path, branch: &str) -> PathBuf {
    let leaf = branch.replace(['/', '\\'], "-");
    cwd.join("..").join(format!("{}-{}", cwd_name(cwd), leaf))
}

fn cwd_name(cwd: &Path) -> String {
    cwd.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("worktree")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_list_parses_into_worktrees() {
        let text = "worktree /repo\nHEAD a1b2c3\nbranch refs/heads/main\n\nworktree /repo-feature\nHEAD d4e5f6\nbranch refs/heads/feature\nlocked reason\n\n";
        let worktrees = parse_worktree_list(text);
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0]["path"], "/repo");
        assert_eq!(worktrees[0]["branch"], "refs/heads/main");
        assert_eq!(worktrees[1]["branch"], "refs/heads/feature");
        assert_eq!(worktrees[1]["locked"], "reason");
    }
}
