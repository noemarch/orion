//! What a tool call in the transcript means for the surface.
//!
//! The Chat Lens draws a turn's parts as cards: prose, folded reasoning, a
//! tool call, or an edit diff. This module is the pure side of that — the
//! mapping from a wire part to the card the view draws, the diff a tool's
//! output (or input) carries, the paths a reader can open, and the bound on
//! how much output the chat shows. The view only draws what this says.

use std::ops::Range;

use crate::chat_entities::{ChatPart, ToolPart};
use serde_json::Value;

/// The bound on the output a tool card shows in the chat.
pub const MAX_TOOL_CHARS: usize = 2000;

/// The bound on the file paths one tool card turns into controls.
pub const MAX_FILE_LINKS: usize = 50;

/// How a tool call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Done,
    Failed,
}

impl ToolStatus {
    /// The status in words, as the card shows it.
    pub fn as_word(self) -> &'static str {
        match self {
            Self::Done => "Done",
            Self::Failed => "Failed",
        }
    }
}

/// One tool call, as the card draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCard {
    /// The tool's name (`Bash`, `Edit`, `Read`).
    pub name: String,
    /// How the call ended.
    pub status: ToolStatus,
    /// The one-line summary the tool reported.
    pub summary: String,
    /// The call's input, as the tool received it.
    pub input: String,
    /// The call's output, as the tool returned it.
    pub output: String,
    /// Whether the arguments were too large to keep.
    pub arguments_omitted: bool,
}

/// One line of an edit diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    /// A line the edit left alone.
    Context(String),
    /// A line the edit added.
    Added(String),
    /// A line the edit removed.
    Removed(String),
}

impl DiffLine {
    /// The line's text, without its sign.
    pub fn text(&self) -> &str {
        match self {
            Self::Context(text) | Self::Added(text) | Self::Removed(text) => text,
        }
    }

    /// The sign the diff gives the line: `+`, `-` or a space.
    pub fn sign(&self) -> &'static str {
        match self {
            Self::Context(_) => " ",
            Self::Added(_) => "+",
            Self::Removed(_) => "-",
        }
    }
}

/// What one part of a turn becomes on the surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartCard {
    /// Prose.
    Text { text: String },
    /// The agent's reasoning, folded away until the reader opens it.
    Thinking { text: String },
    /// A tool call.
    Tool(ToolCard),
    /// A tool call whose work was an edit: the diff it made.
    Diff {
        card: ToolCard,
        lines: Vec<DiffLine>,
    },
    /// A notice from the engine.
    Notice {
        text: String,
        source: Option<String>,
    },
    /// A summary of a compacted conversation.
    Compact { text: String },
    /// A part the shell knows but does not draw as a card of its own.
    Other,
    /// A part this shell does not know.
    Unknown,
}

/// The card one part of a turn becomes.
pub fn part_card(part: &ChatPart) -> PartCard {
    match part {
        ChatPart::Text { text, .. } => PartCard::Text { text: text.clone() },
        ChatPart::Thinking { text } => PartCard::Thinking { text: text.clone() },
        ChatPart::Tool(tool) => {
            let card = tool_card(tool);
            let lines = tool_diff(tool);
            if lines.is_empty() {
                PartCard::Tool(card)
            } else {
                PartCard::Diff { card, lines }
            }
        }
        ChatPart::Notice { text, source } => PartCard::Notice {
            text: text.clone(),
            source: source.clone(),
        },
        ChatPart::Compact { text } => PartCard::Compact { text: text.clone() },
        ChatPart::TaskResult { .. } | ChatPart::Skill { .. } | ChatPart::Image { .. } => {
            PartCard::Other
        }
        _ => PartCard::Unknown,
    }
}

/// The tool call as a card.
pub fn tool_card(tool: &ToolPart) -> ToolCard {
    ToolCard {
        name: tool.name.clone(),
        status: if tool.error {
            ToolStatus::Failed
        } else {
            ToolStatus::Done
        },
        summary: tool.summary.clone(),
        input: tool.input.clone(),
        output: tool.output.clone(),
        arguments_omitted: tool.arguments_omitted,
    }
}

/// The verb a tool's row shows (DESIGN.md §5: caret + verb + object). A tool
/// the table does not know keeps its own id in the verb's place.
pub fn tool_verb(name: &str) -> Option<&'static str> {
    match name {
        "Read" | "NotebookRead" => Some("Read"),
        "Edit" | "MultiEdit" | "NotebookEdit" => Some("Edited"),
        "Write" => Some("Wrote"),
        "Bash" | "BashOutput" => Some("Ran"),
        "Glob" | "Grep" => Some("Searched"),
        "Task" => Some("Delegated"),
        "WebFetch" | "WebSearch" => Some("Fetched"),
        _ => None,
    }
}

/// The row's object: the tool's summary without the tool's own name.
pub fn tool_object(summary: &str, name: &str) -> String {
    let prefix = format!("{name}: ");
    summary.strip_prefix(&prefix).unwrap_or(summary).to_string()
}

/// The diff a tool call carries, and whether it was cut at the bound. A tool
/// that did no edit has no diff.
pub fn tool_diff_bounded(tool: &ToolPart) -> (Vec<DiffLine>, bool) {
    let lines = tool_diff(tool);
    let mut used = 0usize;
    let mut kept = Vec::new();
    let mut clipped = false;
    for line in lines {
        used += line.text().len() + 1;
        if used > MAX_TOOL_CHARS {
            clipped = true;
            break;
        }
        kept.push(line);
    }
    (kept, clipped)
}

/// The diff a tool call carries: the diff its output holds, or the edit its
/// input describes. A tool that did neither has no diff.
pub fn tool_diff(tool: &ToolPart) -> Vec<DiffLine> {
    let parsed = diff_lines(&tool.output);
    if !parsed.is_empty() {
        return parsed;
    }
    if let Some((old, new)) = edit_input(&tool.input) {
        return edit_diff(&old, &new);
    }
    Vec::new()
}

/// The diff in a unified diff: the added and deleted lines with the context
/// the diff carries. Text that is not a diff has no lines.
pub fn diff_lines(output: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut in_hunk = false;
    for line in output.lines() {
        if line.starts_with("@@") {
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            lines.push(DiffLine::Added(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix('-') {
            lines.push(DiffLine::Removed(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix(' ') {
            lines.push(DiffLine::Context(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix('\\') {
            // "\ No newline at end of file": a marker, not a line.
            let _ = rest;
        }
    }
    lines
}

/// The old and new text an Edit-style tool describes.
pub fn edit_input(input: &str) -> Option<(String, String)> {
    let value = serde_json::from_str::<Value>(input).ok()?;
    let old = value.get("old_string")?.as_str()?.to_string();
    let new = value.get("new_string")?.as_str()?.to_string();
    Some((old, new))
}

/// The diff between the old and the new text, with the lines around the
/// change.
pub fn edit_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    for group in diff.grouped_ops(3) {
        for op in group {
            for change in diff.iter_changes(&op) {
                let line = change.to_string();
                let line = line.strip_suffix('\n').unwrap_or(&line).to_string();
                lines.push(match change.tag() {
                    similar::ChangeTag::Insert => DiffLine::Added(line),
                    similar::ChangeTag::Delete => DiffLine::Removed(line),
                    similar::ChangeTag::Equal => DiffLine::Context(line),
                });
            }
        }
    }
    lines
}

/// The output as the chat shows it, and whether it was cut.
pub fn clipped_output(output: &str) -> (String, bool) {
    if output.len() <= MAX_TOOL_CHARS {
        return (output.to_string(), false);
    }
    let cut = output.floor_char_boundary(MAX_TOOL_CHARS);
    (output[..cut].to_string(), true)
}

/// One line of a tool's text, with the file paths it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLine {
    /// The line's text.
    pub text: String,
    /// The paths in the line, with the range each covers.
    pub paths: Vec<(Range<usize>, String)>,
}

/// The lines of a tool's text, each with the file paths it holds. A tool's
/// input and output are read line by line, so the paths are drawn inside the
/// line they sit on.
pub fn lines_with_paths(text: &str) -> Vec<ToolLine> {
    text.split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            ToolLine {
                text: line.to_string(),
                paths: file_path_ranges(line),
            }
        })
        .collect()
}

/// The file paths in a tool's text: the paths a reader can open.
pub fn file_paths(text: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for (_, path) in file_path_ranges(text) {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// The file paths in a tool's text, with the range each one covers, so the
/// view can draw the path as its own control.
pub fn file_path_ranges(text: &str) -> Vec<(Range<usize>, String)> {
    let mut found: Vec<(Range<usize>, String)> = Vec::new();
    let mut push = |range: Range<usize>, path: &str| {
        // The same text found twice (a JSON argument and the token scan) is
        // one path, and the first one found is the real one.
        if found
            .iter()
            .any(|(seen, _)| seen.start < range.end && range.start < seen.end)
        {
            return;
        }
        if !found.iter().any(|(_, seen)| seen == path) {
            found.push((range, path.to_string()));
        }
    };

    // A JSON argument names its file plainly. The text holds the escaped
    // form, so the range covers that and the path stays the real one.
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        for key in ["file_path", "path", "notebook_path", "filePath"] {
            let Some(path) = value.get(key).and_then(Value::as_str) else {
                continue;
            };
            if !is_file_path(path) {
                continue;
            }
            let escaped = serde_json::to_string(path).unwrap_or_default();
            let escaped = escaped.trim_matches('"');
            if let Some(start) = text.find(escaped) {
                push(start..start + escaped.len(), path);
            }
        }
    }

    // The rest of the text: path-like tokens. A colon does not split a
    // token: it is part of a Windows drive letter.
    for (start, token) in tokens(text) {
        let token = token.trim_matches(|character: char| {
            !character.is_alphanumeric()
                && !matches!(character, '.' | '/' | '\\' | ':' | '-' | '_' | '~')
        });
        if token.is_empty() || !is_file_path(token) {
            continue;
        }
        let offset = token.len() - token.trim_start().len();
        let begin = start + offset;
        push(begin..begin + token.len(), token);
    }
    // The paths come back in the order they appear, whatever found them.
    found.sort_by_key(|(range, _)| range.start);
    found
}

/// The text's tokens, with where each one starts. A token ends at whitespace
/// or a quote; a colon does not end one: it is part of a Windows drive
/// letter.
fn tokens(text: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if character.is_whitespace() || "\"'`,;()[]{}<>".contains(character) {
            if index > start {
                found.push((start, &text[start..index]));
            }
            start = index + character.len_utf8();
        }
    }
    if start < text.len() {
        found.push((start, &text[start..]));
    }
    found
}

/// Whether a token names a file: it has a directory and an extension.
fn is_file_path(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.contains("://") {
        // A URL is not a path on this machine.
        return false;
    }
    let last = candidate.rsplit(['/', '\\']).next().unwrap_or(candidate);
    let Some((_, extension)) = last.rsplit_once('.') else {
        return false;
    };
    if extension.is_empty()
        || extension.len() > 8
        || !extension
            .chars()
            .all(|character| character.is_alphanumeric())
    {
        return false;
    }
    candidate.contains('/') || candidate.contains('\\') || candidate.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_entities::ToolPart;
    use serde_json::json;

    fn tool(name: &str, input: &str, output: &str, error: bool) -> ToolPart {
        ToolPart {
            name: name.to_string(),
            summary: "a summary".to_string(),
            input: input.to_string(),
            output: output.to_string(),
            error,
            arguments_omitted: false,
            skill: None,
            output_ref: None,
            output_size: None,
            images: Vec::new(),
        }
    }

    #[test]
    fn prose_and_reasoning_are_not_tool_cards() {
        let text = part_card(&ChatPart::Text {
            text: "hello".to_string(),
            phase: None,
        });
        assert_eq!(
            text,
            PartCard::Text {
                text: "hello".to_string()
            }
        );
        let thinking = part_card(&ChatPart::Thinking {
            text: "hmm".to_string(),
        });
        assert_eq!(
            thinking,
            PartCard::Thinking {
                text: "hmm".to_string()
            }
        );
    }

    #[test]
    fn a_tool_call_becomes_a_card_with_its_name_and_status() {
        let card = part_card(&ChatPart::Tool(tool("Bash", "{}", "done", false)));
        let PartCard::Tool(card) = card else {
            panic!("a tool call is a tool card");
        };
        assert_eq!(card.name, "Bash");
        assert_eq!(card.status, ToolStatus::Done);
        assert_eq!(card.summary, "a summary");
        assert_eq!(card.status, ToolStatus::Done);
    }

    #[test]
    fn a_failed_tool_call_says_so_and_keeps_its_output() {
        let card = part_card(&ChatPart::Tool(tool("Bash", "{}", "boom", true)));
        let PartCard::Tool(card) = card else {
            panic!("a failed tool call is still a tool card");
        };
        assert_eq!(card.status, ToolStatus::Failed);
        assert_eq!(card.output, "boom", "the failure stays on screen");
        assert_eq!(card.status.as_word(), "Failed");
    }

    #[test]
    fn a_tool_that_edited_a_file_becomes_a_diff() {
        let input = json!({
            "file_path": "C:/work/src/main.rs",
            "old_string": "one\ntwo\n",
            "new_string": "one\nthree\n",
        })
        .to_string();
        let card = part_card(&ChatPart::Tool(tool(
            "Edit",
            &input,
            "The file has been updated.",
            false,
        )));
        let PartCard::Diff { card, lines } = card else {
            panic!("an edit is a diff");
        };
        assert_eq!(card.name, "Edit");
        assert!(
            lines
                .iter()
                .any(|line| line == &DiffLine::Added("three".to_string())),
            "the new line is there: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &DiffLine::Removed("two".to_string())),
            "the old line is there: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &DiffLine::Context("one".to_string())),
            "the context around the change is there: {lines:?}"
        );
    }

    #[test]
    fn a_unified_diff_in_the_output_is_read() {
        let output = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "index 111..222 100644\n",
            "--- a/src/main.rs\n",
            "+++ b/src/main.rs\n",
            "@@ -1,3 +1,3 @@\n",
            " one\n",
            "-two\n",
            "+three\n",
        );
        let lines = diff_lines(output);
        assert_eq!(
            lines,
            vec![
                DiffLine::Context("one".to_string()),
                DiffLine::Removed("two".to_string()),
                DiffLine::Added("three".to_string()),
            ]
        );
        assert_eq!(lines[1].sign(), "-");
        assert_eq!(lines[2].sign(), "+");
        assert_eq!(lines[1].text(), "two");
    }

    #[test]
    fn text_that_is_not_a_diff_has_no_diff_lines() {
        assert!(diff_lines("The file has been updated.").is_empty());
        assert!(diff_lines("").is_empty());
    }

    #[test]
    fn a_file_path_in_the_text_is_found() {
        let paths = file_paths("Read C:\\work\\src\\main.rs and src/lib.rs today");
        assert!(
            paths.contains(&"C:\\work\\src\\main.rs".to_string()),
            "{paths:?}"
        );
        assert!(paths.contains(&"src/lib.rs".to_string()), "{paths:?}");
        assert!(
            !paths.iter().any(|path| path == "today"),
            "a word is not a path: {paths:?}"
        );
    }

    #[test]
    fn a_json_argument_names_its_file() {
        let paths = file_paths(&json!({ "file_path": "C:/work/src/main.rs" }).to_string());
        assert_eq!(paths, vec!["C:/work/src/main.rs".to_string()]);
    }

    #[test]
    fn a_file_path_knows_where_it_sits() {
        let text = "Read C:/work/src/main.rs now";
        let ranges = file_path_ranges(text);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].1, "C:/work/src/main.rs");
        assert_eq!(
            &text[ranges[0].0.clone()],
            "C:/work/src/main.rs",
            "the range covers the path in the text"
        );
    }

    #[test]
    fn the_paths_come_back_in_the_order_they_appear() {
        let text = "{\"note\":\"a/c.rs\",\"file_path\":\"src/b.rs\"}";
        let ranges = file_path_ranges(text);
        let starts: Vec<usize> = ranges.iter().map(|(range, _)| range.start).collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(starts, sorted, "the ranges are ordered: {ranges:?}");
    }

    #[test]
    fn a_json_path_is_the_real_path_not_its_escaped_form() {
        let text = "{\"file_path\":\"C:\\\\Users\\\\me\\\\main.rs\"}";
        let paths = file_paths(text);
        assert_eq!(
            paths,
            vec!["C:\\Users\\me\\main.rs".to_string()],
            "the reader opens the real path"
        );
    }

    #[test]
    fn a_url_is_not_a_file_path() {
        let paths = file_paths("see https://example.com/api/workspace/create");
        assert!(paths.is_empty(), "{paths:?}");
    }

    #[test]
    fn a_tool_row_reads_as_verb_and_object() {
        assert_eq!(tool_verb("Bash"), Some("Ran"));
        assert_eq!(tool_verb("Edit"), Some("Edited"));
        assert_eq!(tool_verb("Read"), Some("Read"));
        assert_eq!(tool_verb("Widget"), None, "an unknown tool keeps its id");
        assert_eq!(
            tool_object("Bash: ls -la src", "Bash"),
            "ls -la src",
            "the object is the summary without the tool's name"
        );
        assert_eq!(tool_object("Widget", "Widget"), "Widget");
    }

    #[test]
    fn output_beyond_the_bound_is_cut() {
        let short = "x".repeat(100);
        let (text, clipped) = clipped_output(&short);
        assert_eq!(text, short);
        assert!(!clipped);

        let long = "x".repeat(MAX_TOOL_CHARS + 100);
        let (text, clipped) = clipped_output(&long);
        assert!(clipped, "the card says the output was cut");
        assert_eq!(text.len(), MAX_TOOL_CHARS);
    }
}
