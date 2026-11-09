//! Markdown → styled lines for the chat lens (P2).
//!
//! [`parse_markdown`] is a pure function: pulldown-cmark events are folded into
//! one [`MarkdownLine`] per block line, each carrying inline runs (byte ranges +
//! [`RunKind`]) the renderer maps onto `HighlightStyle`/`TextStyle`. Syntax
//! highlighting is deliberately absent: code blocks render plain mono with a
//! block background (`BlockKind::Code`).

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use std::ops::Range;

/// Block style of one output line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Heading(u8),
    Paragraph,
    Code,
    Quote,
    ListItem(u8),
    Rule,
    Text,
}

/// One inline run (bold/italic/code/link) inside a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Bold,
    Italic,
    Code,
    Link,
}

/// One rendered line of markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownLine {
    pub text: String,
    pub style: BlockKind,
    pub runs: Vec<(Range<usize>, RunKind)>,
}

impl MarkdownLine {
    fn blank(style: BlockKind) -> Self {
        Self {
            text: String::new(),
            style,
            runs: Vec::new(),
        }
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Fold markdown source into styled lines. Never panics on malformed input;
/// unterminated blocks flush at EOF.
pub fn parse_markdown(source: &str) -> Vec<MarkdownLine> {
    let mut lines: Vec<MarkdownLine> = Vec::new();
    let mut current: Option<MarkdownLine> = None;
    let mut inline: Vec<RunKind> = Vec::new();
    let mut list_depth: u8 = 0;
    let mut in_table_cell = false;
    let mut code_depth = 0usize;
    let mut quote_depth = 0usize;

    fn ensure(lines: &mut Vec<MarkdownLine>, current: &mut Option<MarkdownLine>, style: BlockKind) {
        let _ = lines;
        if current.is_none() {
            *current = Some(MarkdownLine::blank(style));
        }
    }

    fn flush(lines: &mut Vec<MarkdownLine>, current: &mut Option<MarkdownLine>) {
        if let Some(line) = current.take() {
            if !line.text.is_empty() || line.style == BlockKind::Rule {
                lines.push(line);
            }
        }
    }

    fn append(line: &mut MarkdownLine, text: &str, inline: &[RunKind]) {
        let start = line.text.len();
        line.text.push_str(text);
        if let Some(&run) = inline.last() {
            line.runs.push((start..line.text.len(), run));
        }
    }

    fn append_split(
        lines: &mut Vec<MarkdownLine>,
        current: &mut Option<MarkdownLine>,
        text: &str,
        inline: &[RunKind],
        default_style: BlockKind,
    ) {
        for (index, fragment) in text.split('\n').enumerate() {
            if index > 0 {
                flush(lines, current);
            }
            if fragment.is_empty() {
                continue;
            }
            if current.is_none() {
                *current = Some(MarkdownLine::blank(default_style.clone()));
            }
            if let Some(line) = current.as_mut() {
                append(line, fragment, inline);
            }
        }
    }

    let parser = Parser::new(source);
    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    flush(&mut lines, &mut current);
                    current = Some(MarkdownLine::blank(BlockKind::Heading(heading_level(
                        level,
                    ))));
                }
                Tag::Paragraph => {
                    // Inside a block quote, the Quote-styled pending line must survive.
                    if quote_depth == 0 {
                        flush(&mut lines, &mut current);
                    }
                    ensure(&mut lines, &mut current, BlockKind::Paragraph);
                }
                Tag::CodeBlock(_) => {
                    flush(&mut lines, &mut current);
                    code_depth += 1;
                    ensure(&mut lines, &mut current, BlockKind::Code);
                }
                Tag::BlockQuote(_) => {
                    flush(&mut lines, &mut current);
                    quote_depth += 1;
                    ensure(&mut lines, &mut current, BlockKind::Quote);
                }
                Tag::List(_) => list_depth = list_depth.saturating_add(1),
                Tag::Item => {
                    flush(&mut lines, &mut current);
                    let depth = list_depth.saturating_sub(1).min(8);
                    let mut line = MarkdownLine::blank(BlockKind::ListItem(depth));
                    line.text.push_str("  ".repeat(depth as usize).as_str());
                    line.text.push_str(if depth == 0 { "- " } else { "* " });
                    current = Some(line);
                }
                Tag::Table(_) => {
                    flush(&mut lines, &mut current);
                    in_table_cell = true;
                    ensure(&mut lines, &mut current, BlockKind::Text);
                }
                Tag::TableCell => {
                    in_table_cell = true;
                    ensure(&mut lines, &mut current, BlockKind::Text);
                }
                Tag::Strong => inline.push(RunKind::Bold),
                Tag::Emphasis => inline.push(RunKind::Italic),
                Tag::Strikethrough => inline.push(RunKind::Italic),
                Tag::Link { .. } | Tag::Image { .. } => inline.push(RunKind::Link),
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph | TagEnd::Heading(_) => flush(&mut lines, &mut current),
                TagEnd::BlockQuote(..) => {
                    quote_depth = quote_depth.saturating_sub(1);
                    flush(&mut lines, &mut current);
                }
                TagEnd::CodeBlock => {
                    code_depth = code_depth.saturating_sub(1);
                    flush(&mut lines, &mut current);
                }
                TagEnd::List(_) => list_depth = list_depth.saturating_sub(1),
                TagEnd::Item | TagEnd::TableRow => flush(&mut lines, &mut current),
                TagEnd::TableCell => {
                    if let Some(line) = current.as_mut() {
                        line.text.push_str(" | ");
                    }
                    in_table_cell = false;
                }
                TagEnd::Strong
                | TagEnd::Emphasis
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image => {
                    inline.pop();
                }
                _ => {}
            },
            Event::Text(text) => {
                let style = if code_depth > 0 {
                    BlockKind::Code
                } else if in_table_cell {
                    BlockKind::Text
                } else if quote_depth > 0 {
                    BlockKind::Quote
                } else {
                    BlockKind::Paragraph
                };
                append_split(&mut lines, &mut current, &text, &inline, style);
            }
            Event::Code(text) => {
                if current.is_none() {
                    current = Some(MarkdownLine::blank(BlockKind::Paragraph));
                }
                if let Some(line) = current.as_mut() {
                    append(line, &text, &[RunKind::Code]);
                }
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                if current.is_none() {
                    current = Some(MarkdownLine::blank(BlockKind::Paragraph));
                }
                if let Some(line) = current.as_mut() {
                    append(line, &text, &[RunKind::Code]);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(line) = current.as_mut() {
                    line.text.push(' ');
                }
            }
            Event::Rule => {
                flush(&mut lines, &mut current);
                lines.push(MarkdownLine {
                    text: "---".to_string(),
                    style: BlockKind::Rule,
                    runs: Vec::new(),
                });
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                append_split(&mut lines, &mut current, &html, &inline, BlockKind::Text);
            }
            Event::FootnoteReference(label) => {
                if current.is_none() {
                    current = Some(MarkdownLine::blank(BlockKind::Paragraph));
                }
                if let Some(line) = current.as_mut() {
                    let note = format!("[^{label}]");
                    append(line, &note, &inline);
                }
            }
            Event::TaskListMarker(checked) => {
                if let Some(line) = current.as_mut() {
                    let mark = if checked { "[x] " } else { "[ ] " };
                    line.text.push_str(mark);
                }
            }
        }
    }
    flush(&mut lines, &mut current);
    // Empty input produces no lines; whitespace-only text lines are trimmed.
    for line in &mut lines {
        line.text = line.text.trim_end().to_string();
    }
    lines.retain(|line| line.style == BlockKind::Rule || !line.text.trim().is_empty());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_and_paragraph() {
        let lines = parse_markdown("# Title\n\nHello world.\n");
        assert!(lines
            .iter()
            .any(|l| l.style == BlockKind::Heading(1) && l.text == "Title"));
        assert!(lines
            .iter()
            .any(|l| l.style == BlockKind::Paragraph && l.text == "Hello world."));
    }

    #[test]
    fn inline_runs_bold_code_link() {
        let lines = parse_markdown("A **bold** and `code` and [link](https://example.com).\n");
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line.text, "A bold and code and link.");
        assert!(line.runs.iter().any(|(_, k)| *k == RunKind::Bold));
        assert!(line.runs.iter().any(|(_, k)| *k == RunKind::Code));
        assert!(line.runs.iter().any(|(_, k)| *k == RunKind::Link));
        for (range, kind) in &line.runs {
            assert_eq!(
                &line.text[range.clone()],
                match kind {
                    RunKind::Bold => "bold",
                    RunKind::Code => "code",
                    RunKind::Link => "link",
                    RunKind::Italic => unreachable!("no italic in this input"),
                }
            );
        }
    }

    #[test]
    fn list_and_code_block() {
        let lines = parse_markdown("- one\n- two\n\n```\nlet x = 1;\n```\n");
        assert!(lines
            .iter()
            .any(|l| l.style == BlockKind::ListItem(0) && l.text.contains("one")));
        assert!(lines
            .iter()
            .any(|l| l.style == BlockKind::Code && l.text.contains("let x = 1;")));
    }

    #[test]
    fn quote_and_rule() {
        let lines = parse_markdown("> quoted\n\n---\n");
        assert!(lines.iter().any(|l| l.style == BlockKind::Quote));
        assert!(lines.iter().any(|l| l.style == BlockKind::Rule));
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(parse_markdown("").is_empty());
        assert!(parse_markdown("\n\n").is_empty());
    }
}
