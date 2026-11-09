//! Typed wire structs for the files/changes surfaces (P3).
//!
//! The frozen wire contract is `shared/protocol.ts`: `DirectoryListing`,
//! `RepoDiffFileList`, `RepoDiffFilePatch`, and `FileInfo`. These structs
//! parse that wire shape with serde and are the only place the GPUI views
//! learn the JSON layout. Unknown future fields are ignored.
//!
//! Wire shapes (frozen, `shared/protocol.ts`):
//! - `DirectoryListing { path, parent, home, directories[], truncated, files?: [{name, size}] }`
//! - `RepoDiffRepository { root, branch, head, detached, unborn }`
//! - `RepoDiffFile { path, old_path, status, additions, deletions, binary }`
//! - `RepoDiffFileList { repository, scope, base, head, files[], total, truncated, limits }`
//! - `RepoDiffFilePatch { repository, scope, base, head, file, patch: {patch_text, from_head...} | null, unavailable, truncated, limits }`
//!   The web client reads the patch text from `patch` (git's own output for
//!   the one file, from its `diff --git` line on, either complete or null);
//!   `unavailable` says why there is none (`binary`/`too_large`/`unmerged`/
//!   `unsupported`, null when `patch` is set).
//! - `FileInfo { path, name, size, modified, mime, kind: image|video|audio|pdf|text|binary }`

use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// DirectoryListing — GET /api/workspace/directories
// ---------------------------------------------------------------------------

/// One file beside the folders in a [`DirectoryListing`], when asked for (`files=1`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DirectoryFile {
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

/// `GET /api/workspace/directories`: the folders (and optionally files) in one directory.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DirectoryListing {
    /// The directory listed, absolute.
    pub path: String,
    /// Its parent, or null at the root.
    #[serde(default)]
    pub parent: Option<String>,
    /// The user's home, so the browser can show `~` and offer a way back.
    #[serde(default)]
    pub home: String,
    /// Folder names, sorted.
    #[serde(default)]
    pub directories: Vec<String>,
    /// More entries than the list holds.
    #[serde(default)]
    pub truncated: bool,
    /// The files beside the folders, when asked for (`files=1`).
    #[serde(default)]
    pub files: Option<Vec<DirectoryFile>>,
}

impl DirectoryListing {
    /// Parse the route's JSON body (the `Value` returned by [`orion_core::Engine::http`]).
    pub fn parse(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    /// The files beside the folders, empty when not asked for.
    pub fn file_entries(&self) -> &[DirectoryFile] {
        self.files.as_deref().unwrap_or(&[])
    }
}

/// Join a folder/file `name` onto a directory `path` with `/` separators,
/// matching how the wire names children (relative to the listed directory).
pub fn join_path(path: &str, name: &str) -> String {
    if path.ends_with('/') {
        format!("{path}{name}")
    } else {
        format!("{path}/{name}")
    }
}

// ---------------------------------------------------------------------------
// RepoDiff — GET /api/workspace/diff/files and /api/workspace/diff/file
// ---------------------------------------------------------------------------

/// Which two states of a repository are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoDiffScope {
    Workspace,
    Staged,
    Commit,
}

impl RepoDiffScope {
    pub fn as_str(self) -> &'static str {
        match self {
            RepoDiffScope::Workspace => "workspace",
            RepoDiffScope::Staged => "staged",
            RepoDiffScope::Commit => "commit",
        }
    }

    /// Parse a wire scope string; unknown scopes fall back to `workspace`.
    pub fn from_wire(value: &str) -> Self {
        match value {
            "staged" => RepoDiffScope::Staged,
            "commit" => RepoDiffScope::Commit,
            _ => RepoDiffScope::Workspace,
        }
    }
}

/// How one file changed. `untracked` exists only in the `workspace` scope.
/// An unknown future status deserializes as [`RepoDiffStatus::Unknown`] so a
/// newer server never blanks the list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoDiffStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
    #[serde(other)]
    #[default]
    Unknown,
}

impl RepoDiffStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RepoDiffStatus::Added => "added",
            RepoDiffStatus::Deleted => "deleted",
            RepoDiffStatus::Modified => "modified",
            RepoDiffStatus::Renamed => "renamed",
            RepoDiffStatus::Copied => "copied",
            RepoDiffStatus::TypeChanged => "type_changed",
            RepoDiffStatus::Unmerged => "unmerged",
            RepoDiffStatus::Untracked => "untracked",
            RepoDiffStatus::Unknown => "unknown",
        }
    }

    /// Single-letter mark the list shows for the status (git porcelain style).
    pub fn mark(self) -> &'static str {
        match self {
            RepoDiffStatus::Added => "A",
            RepoDiffStatus::Deleted => "D",
            RepoDiffStatus::Renamed => "R",
            RepoDiffStatus::Copied => "C",
            RepoDiffStatus::Untracked => "?",
            RepoDiffStatus::Unmerged => "U",
            RepoDiffStatus::TypeChanged => "T",
            RepoDiffStatus::Modified => "M",
            RepoDiffStatus::Unknown => "·",
        }
    }
}

/// The repository a comparison was read from.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct RepoDiffRepository {
    /// The working tree's top folder, absolute.
    #[serde(default)]
    pub root: String,
    /// The checked-out branch; null on a detached HEAD.
    #[serde(default)]
    pub branch: Option<String>,
    /// HEAD's commit hash; null before the first commit.
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub detached: bool,
    /// No commit yet: `staged` then lists everything added so far.
    #[serde(default)]
    pub unborn: bool,
}

/// One file that changed in a comparison.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RepoDiffFile {
    /// Relative to the repository root, forward slashes, as git names it.
    pub path: String,
    /// Where a renamed or copied file came from; null otherwise.
    #[serde(default)]
    pub old_path: Option<String>,
    #[serde(default)]
    pub status: RepoDiffStatus,
    /// Lines added and removed; null for a binary file, an untracked one,
    /// and past the list's byte cap.
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    /// Null when not determined (an untracked file in a list is not opened).
    #[serde(default)]
    pub binary: Option<bool>,
}

/// Limits echoed by the diff routes.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub struct RepoDiffLimits {
    #[serde(default)]
    pub max_files: Option<u64>,
    #[serde(default)]
    pub max_list_bytes: Option<u64>,
    #[serde(default)]
    pub max_patch_bytes: Option<u64>,
    #[serde(default)]
    pub context_lines: Option<u64>,
}

/// `GET /api/workspace/diff/files`: the files a comparison changed.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RepoDiffFileList {
    #[serde(default)]
    pub repository: RepoDiffRepository,
    #[serde(default)]
    pub scope: Option<String>,
    /// The commits compared, as full hashes, in the `commit` scope; null otherwise.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub files: Vec<RepoDiffFile>,
    /// How many files changed in all; null when git printed more than the cap.
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub limits: RepoDiffLimits,
}

impl RepoDiffFileList {
    /// Parse the route's JSON body.
    pub fn parse(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }
}

/// `GET /api/workspace/diff/file`: one changed file's patch, or why there is none.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RepoDiffFilePatch {
    #[serde(default)]
    pub repository: RepoDiffRepository,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    pub file: RepoDiffFile,
    /// Git's own output for that one file, from its `diff --git` line on;
    /// either complete or null.
    #[serde(default)]
    pub patch: Option<String>,
    /// Why there is no patch: `binary`, `too_large`, `unmerged`, or
    /// `unsupported`. Null when `patch` is set.
    #[serde(default)]
    pub unavailable: Option<String>,
    /// The patch exists and was left out for its size.
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub limits: RepoDiffLimits,
}

impl RepoDiffFilePatch {
    /// Parse the route's JSON body.
    pub fn parse(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    /// The raw unified-diff text, when there is one.
    pub fn patch_text(&self) -> Option<&str> {
        self.patch.as_deref()
    }
}

// ---------------------------------------------------------------------------
// FileInfo — GET /api/fs/stat
// ---------------------------------------------------------------------------

/// How the file viewer shows a file. An unknown future kind deserializes as
/// [`FileKind::Other`] so the reader falls back to the text path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Image,
    Video,
    Audio,
    Pdf,
    Text,
    Binary,
    #[serde(other)]
    #[default]
    Other,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Image => "image",
            FileKind::Video => "video",
            FileKind::Audio => "audio",
            FileKind::Pdf => "pdf",
            FileKind::Text => "text",
            FileKind::Binary => "binary",
            FileKind::Other => "other",
        }
    }

    /// True when the reader shows a text body for this kind.
    pub fn is_textual(self) -> bool {
        matches!(self, FileKind::Text | FileKind::Other)
    }
}

/// `GET /api/fs/stat`: a file the viewer can open.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct FileInfo {
    /// The file's path, absolute.
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
    /// ISO time of the last change.
    #[serde(default)]
    pub modified: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub kind: FileKind,
}

impl FileInfo {
    /// Parse the route's JSON body.
    pub fn parse(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }
}

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

/// Percent-encode one query value (the route table percent-decodes it).
/// Unreserved characters pass through; everything else becomes `%XX` over
/// the value's UTF-8 bytes.
pub fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_directory_listing() {
        let value = json!({
            "path": "/home/u/proj",
            "parent": "/home/u",
            "home": "/home/u",
            "directories": ["src", "docs"],
            "truncated": false,
            "files": [{"name": "README.md", "size": 42}],
        });
        let listing = DirectoryListing::parse(&value).expect("parse");
        assert_eq!(listing.path, "/home/u/proj");
        assert_eq!(listing.parent.as_deref(), Some("/home/u"));
        assert_eq!(listing.home, "/home/u");
        assert_eq!(listing.directories, vec!["src", "docs"]);
        assert!(!listing.truncated);
        assert_eq!(listing.file_entries()[0].name, "README.md");
        assert_eq!(listing.file_entries()[0].size, 42);
    }

    #[test]
    fn parses_directory_listing_without_files_or_parent() {
        let value = json!({
            "path": "/", "parent": null, "home": "/home/u",
            "directories": [], "truncated": true,
        });
        let listing = DirectoryListing::parse(&value).expect("parse");
        assert_eq!(listing.parent, None);
        assert!(listing.file_entries().is_empty());
        assert!(listing.truncated);
    }

    #[test]
    fn parses_diff_file_list() {
        let value = json!({
            "repository": {"root": "/r", "branch": "main", "head": "abc", "detached": false, "unborn": false},
            "scope": "workspace",
            "base": null, "head": null,
            "files": [
                {"path": "a.rs", "old_path": null, "status": "modified", "additions": 3, "deletions": 1, "binary": null},
                {"path": "b.png", "old_path": null, "status": "added", "additions": null, "deletions": null, "binary": true},
                {"path": "c.txt", "old_path": null, "status": "untracked", "additions": null, "deletions": null, "binary": null},
                {"path": "d.rs", "old_path": "e.rs", "status": "renamed", "additions": 0, "deletions": 0, "binary": null},
            ],
            "total": 4, "truncated": false,
            "limits": {"max_files": 2000, "max_list_bytes": 1048576},
        });
        let list = RepoDiffFileList::parse(&value).expect("parse");
        assert_eq!(list.repository.branch.as_deref(), Some("main"));
        assert_eq!(list.files.len(), 4);
        assert_eq!(list.files[0].status, RepoDiffStatus::Modified);
        assert_eq!(list.files[0].additions, Some(3));
        assert_eq!(list.files[1].binary, Some(true));
        assert_eq!(list.files[2].status, RepoDiffStatus::Untracked);
        assert_eq!(list.files[3].status, RepoDiffStatus::Renamed);
        assert_eq!(list.files[3].old_path.as_deref(), Some("e.rs"));
        assert_eq!(list.total, Some(4));
    }

    #[test]
    fn parses_diff_file_patch() {
        let value = json!({
            "repository": {"root": "/r", "branch": null, "head": null, "detached": true, "unborn": true},
            "scope": "staged", "base": null, "head": null,
            "file": {"path": "a.rs", "old_path": null, "status": "modified", "additions": null, "deletions": null, "binary": null},
            "patch": "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "unavailable": null, "truncated": false,
            "limits": {"max_patch_bytes": 262144},
        });
        let patch = RepoDiffFilePatch::parse(&value).expect("parse");
        assert_eq!(patch.file.path, "a.rs");
        assert!(patch.patch_text().unwrap().contains("+new"));
        assert_eq!(patch.unavailable, None);
        assert_eq!(patch.limits.max_patch_bytes, Some(262144));
    }

    #[test]
    fn parses_diff_file_patch_unavailable() {
        let value = json!({
            "repository": {"root": "/r", "branch": "main", "head": "h", "detached": false, "unborn": false},
            "scope": "workspace", "base": null, "head": null,
            "file": {"path": "b.png", "old_path": null, "status": "modified", "additions": null, "deletions": null, "binary": true},
            "patch": null, "unavailable": "binary", "truncated": false,
            "limits": {"max_patch_bytes": 262144},
        });
        let patch = RepoDiffFilePatch::parse(&value).expect("parse");
        assert_eq!(patch.patch_text(), None);
        assert_eq!(patch.unavailable.as_deref(), Some("binary"));
    }

    #[test]
    fn parses_file_info() {
        let value = json!({
            "path": "/r/a.rs", "name": "a.rs", "size": 123,
            "modified": "2026-01-01T00:00:00Z", "mime": "text/plain", "kind": "text",
        });
        let info = FileInfo::parse(&value).expect("parse");
        assert_eq!(info.name, "a.rs");
        assert_eq!(info.size, 123);
        assert_eq!(info.kind, FileKind::Text);
        assert!(info.kind.is_textual());
    }

    #[test]
    fn parses_file_info_binary_and_unknown_kind() {
        let binary = FileInfo::parse(&json!({"path": "/r/b.bin", "name": "b.bin", "size": 9, "modified": "", "mime": "application/octet-stream", "kind": "binary"})).expect("parse");
        assert_eq!(binary.kind, FileKind::Binary);
        assert!(!binary.kind.is_textual());
        let unknown = FileInfo::parse(&json!({"path": "/r/x", "name": "x", "size": 1, "modified": "", "mime": "text/plain", "kind": "future"})).expect("parse");
        assert_eq!(unknown.kind, FileKind::Other);
    }

    #[test]
    fn parses_unknown_status_as_unknown() {
        let value = json!({"path": "z", "old_path": null, "status": "conflicted", "additions": null, "deletions": null, "binary": null});
        let file: RepoDiffFile = serde_json::from_value(value).expect("parse");
        assert_eq!(file.status, RepoDiffStatus::Unknown);
        assert_eq!(file.status.mark(), "·");
    }

    #[test]
    fn scope_wire_round_trip() {
        assert_eq!(RepoDiffScope::from_wire("staged"), RepoDiffScope::Staged);
        assert_eq!(RepoDiffScope::from_wire("commit"), RepoDiffScope::Commit);
        assert_eq!(
            RepoDiffScope::from_wire("workspace"),
            RepoDiffScope::Workspace
        );
        assert_eq!(RepoDiffScope::from_wire("bogus"), RepoDiffScope::Workspace);
        assert_eq!(RepoDiffScope::Workspace.as_str(), "workspace");
    }

    #[test]
    fn join_path_handles_trailing_slash() {
        assert_eq!(join_path("/a/b", "c"), "/a/b/c");
        assert_eq!(join_path("/a/b/", "c"), "/a/b/c");
    }

    #[test]
    fn url_encode_encodes_special_bytes() {
        assert_eq!(url_encode("a b/c?d&e"), "a%20b%2Fc%3Fd%26e");
        assert_eq!(url_encode("plain-1_2.3~"), "plain-1_2.3~");
    }
}
