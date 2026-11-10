//! Markdown → styled lines for the chat lens.
//!
//! [`parse_markdown`] is a pure function: pulldown-cmark events are folded into
//! one [`MarkdownLine`] per block line, each carrying inline runs (byte ranges +
//! [`RunKind`]) and the link targets those runs point at. [`spans`] splits a
//! line into the slices the renderer draws, [`block_style`] says what shape a
//! block has, and the sizes come from [`crate::theme`] (the design's scale).
//! Syntax highlighting is deliberately absent: code renders plain mono.

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

impl BlockKind {
    /// Whether the block is a fenced code block.
    pub fn is_code(&self) -> bool {
        matches!(self, BlockKind::Code)
    }

    /// Whether the block is a horizontal rule.
    pub fn is_rule(&self) -> bool {
        matches!(self, BlockKind::Rule)
    }

    /// Whether the block is a list item, and how deep it sits.
    pub fn list_depth(&self) -> Option<u8> {
        match self {
            BlockKind::ListItem(depth) => Some(*depth),
            _ => None,
        }
    }
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
    /// The link targets in this line, with the range of the text they cover.
    pub links: Vec<(Range<usize>, String)>,
}

impl MarkdownLine {
    fn blank(style: BlockKind) -> Self {
        Self {
            text: String::new(),
            style,
            runs: Vec::new(),
            links: Vec::new(),
        }
    }
}

/// How one markdown block is drawn: the Quiet Slate shape of the line. The
/// sizes live in [`crate::theme`], which owns the design's scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockStyle {
    /// Whether the line is bold.
    pub bold: bool,
    /// Whether the line is monospaced.
    pub mono: bool,
    /// Indent from the left edge, in px.
    pub indent: f32,
    /// Whether the line is a quote: quiet, with a rule at its left.
    pub quote: bool,
    /// Whether the line is a divider.
    pub rule: bool,
}

/// One indent step for a nested list item, in px.
pub const LIST_INDENT: f32 = 16.0;

/// The shape of one block kind: headings grow, code quiets down, lists indent,
/// quotes and rules keep their own shape.
pub fn block_style(kind: &BlockKind) -> BlockStyle {
    let mut style = BlockStyle {
        bold: false,
        mono: false,
        indent: 0.0,
        quote: false,
        rule: false,
    };
    match kind {
        BlockKind::Heading(_) => style.bold = true,
        BlockKind::Code => style.mono = true,
        BlockKind::Quote => style.quote = true,
        BlockKind::ListItem(depth) => style.indent = LIST_INDENT * f32::from(*depth),
        BlockKind::Rule => style.rule = true,
        BlockKind::Paragraph | BlockKind::Text => {}
    }
    style
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
    // The target of the link being read, if it has one.
    let mut link_target: Option<String> = None;

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

    fn append(
        line: &mut MarkdownLine,
        text: &str,
        inline: &[RunKind],
        link_target: &Option<String>,
    ) {
        let start = line.text.len();
        line.text.push_str(text);
        if let Some(&run) = inline.last() {
            let range = start..line.text.len();
            line.runs.push((range.clone(), run));
            if run == RunKind::Link {
                // A link keeps its target so the renderer can open it.
                if let Some(target) = link_target {
                    line.links.push((range, target.clone()));
                }
            }
        }
    }

    fn append_split(
        lines: &mut Vec<MarkdownLine>,
        current: &mut Option<MarkdownLine>,
        text: &str,
        inline: &[RunKind],
        default_style: BlockKind,
        link_target: &Option<String>,
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
                append(line, fragment, inline, link_target);
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
                Tag::Link { dest_url, .. } => {
                    inline.push(RunKind::Link);
                    link_target = Some(dest_url.to_string());
                }
                Tag::Image { dest_url, .. } => {
                    inline.push(RunKind::Link);
                    link_target = Some(dest_url.to_string());
                }
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
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough => {
                    inline.pop();
                }
                TagEnd::Link | TagEnd::Image => {
                    inline.pop();
                    link_target = None;
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
                append_split(
                    &mut lines,
                    &mut current,
                    &text,
                    &inline,
                    style,
                    &link_target,
                );
            }
            Event::Code(text) => {
                if current.is_none() {
                    current = Some(MarkdownLine::blank(BlockKind::Paragraph));
                }
                if let Some(line) = current.as_mut() {
                    append(line, &text, &[RunKind::Code], &None);
                }
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                if current.is_none() {
                    current = Some(MarkdownLine::blank(BlockKind::Paragraph));
                }
                if let Some(line) = current.as_mut() {
                    append(line, &text, &[RunKind::Code], &None);
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
                    links: Vec::new(),
                });
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                append_split(
                    &mut lines,
                    &mut current,
                    &html,
                    &inline,
                    BlockKind::Text,
                    &link_target,
                );
            }
            Event::FootnoteReference(label) => {
                if current.is_none() {
                    current = Some(MarkdownLine::blank(BlockKind::Paragraph));
                }
                if let Some(line) = current.as_mut() {
                    let note = format!("[^{label}]");
                    append(line, &note, &inline, &link_target);
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

/// The text of a list item without its marker, and how many bytes the marker
/// took: the renderer draws the bullet and shifts the inline runs with it.
pub fn list_item_parts(text: &str) -> (&str, usize) {
    let trimmed = text.trim_start();
    let indent = text.len() - trimmed.len();
    for marker in ["- ", "* "] {
        if let Some(body) = trimmed.strip_prefix(marker) {
            return (body, indent + marker.len());
        }
    }
    (trimmed, indent)
}

/// One span of a line: a slice of the text with the style it wears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSpan {
    /// The span's slice of the line's text.
    pub range: Range<usize>,
    /// Whether the span is inline code.
    pub code: bool,
    /// Whether the span is a link.
    pub link: bool,
    /// The link's target, when it has one.
    pub target: Option<String>,
    /// Whether the span is bold.
    pub bold: bool,
    /// Whether the span is italic.
    pub italic: bool,
}

/// Split one line into the spans its inline runs describe.
///
/// A span never ends in whitespace: the space between two spans belongs to the
/// span that follows it, so drawing the spans side by side keeps the prose
/// intact.
pub fn spans(line: &MarkdownLine) -> Vec<TextSpan> {
    let mut cuts: Vec<usize> = vec![0, line.text.len()];
    for (range, _) in &line.runs {
        cuts.push(range.start.min(line.text.len()));
        cuts.push(range.end.min(line.text.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut spans: Vec<TextSpan> = Vec::new();
    for window in cuts.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start == end {
            continue;
        }
        let mut span = TextSpan {
            range: start..end,
            code: false,
            link: false,
            target: None,
            bold: false,
            italic: false,
        };
        for (range, kind) in &line.runs {
            if range.start <= start && end <= range.end {
                match kind {
                    RunKind::Bold => span.bold = true,
                    RunKind::Italic => span.italic = true,
                    RunKind::Code => span.code = true,
                    RunKind::Link => span.link = true,
                }
            }
        }
        span.target = line
            .links
            .iter()
            .find(|(range, _)| range.start <= start && end <= range.end)
            .map(|(_, target)| target.clone());
        match spans.last_mut() {
            Some(last)
                if last.code == span.code
                    && last.link == span.link
                    && last.bold == span.bold
                    && last.italic == span.italic
                    && last.target == span.target =>
            {
                last.range.end = end;
            }
            _ => spans.push(span),
        }
    }

    // Whitespace belongs to the span that follows it, never to the one before.
    for index in 1..spans.len() {
        let previous_start = spans[index - 1].range.start;
        let mut boundary = spans[index].range.start;
        while boundary > previous_start
            && line
                .text
                .as_bytes()
                .get(boundary - 1)
                .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            boundary -= 1;
        }
        if boundary != spans[index].range.start {
            spans[index - 1].range.end = boundary;
            spans[index].range.start = boundary;
        }
    }
    spans
}

/// Whether a link target is one the shell hands to the browser. An in-page
/// anchor or a local file stays inside the app.
pub fn opens_in_browser(target: &str) -> bool {
    let target = target.trim();
    target.starts_with("https://") || target.starts_with("http://") || target.starts_with("mailto:")
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

    #[test]
    fn headings_are_bold_and_prose_is_not() {
        let h1 = block_style(&BlockKind::Heading(1));
        let body = block_style(&BlockKind::Paragraph);
        assert!(h1.bold, "headings are bold");
        assert!(!body.bold, "prose is not bold");
        assert!(!h1.mono && !body.mono, "prose is not monospaced");
    }

    #[test]
    fn code_blocks_are_mono() {
        let code = block_style(&BlockKind::Code);
        assert!(code.mono, "code is monospaced");
    }

    #[test]
    fn list_items_indent_with_their_depth() {
        let shallow = block_style(&BlockKind::ListItem(0));
        let deep = block_style(&BlockKind::ListItem(2));
        assert!(deep.indent > shallow.indent, "nested items sit further in");
        assert_eq!(shallow.indent, 0.0);
    }

    #[test]
    fn a_block_kind_says_what_it_is() {
        assert!(BlockKind::Code.is_code());
        assert!(!BlockKind::Paragraph.is_code());
        assert!(BlockKind::Rule.is_rule());
        assert_eq!(BlockKind::ListItem(2).list_depth(), Some(2));
        assert_eq!(BlockKind::Paragraph.list_depth(), None);
    }

    #[test]
    fn quotes_and_rules_are_marked_as_such() {
        assert!(block_style(&BlockKind::Quote).quote);
        assert!(block_style(&BlockKind::Rule).rule);
        assert!(!block_style(&BlockKind::Paragraph).quote);
    }

    #[test]
    fn a_link_keeps_its_target() {
        let lines = parse_markdown("See [the docs](https://example.com/docs).");
        let link = lines[0].links.first().expect("the link keeps a target");
        assert_eq!(link.1, "https://example.com/docs");
        assert_eq!(&lines[0].text[link.0.clone()], "the docs");
    }

    #[test]
    fn only_web_targets_are_opened() {
        assert!(opens_in_browser("https://example.com/docs"));
        assert!(opens_in_browser("mailto:me@example.com"));
        assert!(
            !opens_in_browser("#anchor"),
            "an in-page anchor is not something to hand to the browser"
        );
        assert!(
            !opens_in_browser("file:///C:/tmp"),
            "a local file is not something to hand to the browser"
        );
    }

    #[test]
    fn a_list_item_loses_its_marker() {
        let (body, shift) = list_item_parts("- one");
        assert_eq!(body, "one");
        assert_eq!(shift, 2);
        let (nested, shift) = list_item_parts("  * two");
        assert_eq!(nested, "two");
        assert_eq!(shift, 4);
        let (plain, shift) = list_item_parts("three");
        assert_eq!(plain, "three");
        assert_eq!(shift, 0);
    }

    #[test]
    fn a_line_splits_into_its_styled_spans() {
        let lines = parse_markdown("A **bold** and `code` and [link](https://example.com).");
        let line = &lines[0];
        let spans = spans(line);
        let kinds: Vec<(String, bool, bool, bool, bool)> = spans
            .iter()
            .map(|span| {
                (
                    line.text[span.range.clone()].to_string(),
                    span.code,
                    span.link,
                    span.bold,
                    span.italic,
                )
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("A".to_string(), false, false, false, false),
                (" bold".to_string(), false, false, true, false),
                (" and".to_string(), false, false, false, false),
                (" code".to_string(), true, false, false, false),
                (" and".to_string(), false, false, false, false),
                (" link".to_string(), false, true, false, false),
                (".".to_string(), false, false, false, false),
            ]
        );
    }

    #[test]
    fn a_link_span_keeps_its_target() {
        let lines = parse_markdown("[docs](https://example.com/docs)");
        let link = spans(&lines[0])
            .into_iter()
            .find(|span| span.link)
            .expect("the link span");
        assert_eq!(link.target.as_deref(), Some("https://example.com/docs"));
    }

    #[test]
    fn no_span_ends_in_whitespace() {
        let lines = parse_markdown("word **bold** more");
        let line = &lines[0];
        for span in spans(line) {
            let text = &line.text[span.range.clone()];
            assert!(
                !text.ends_with(' ') || span.range.end == line.text.len(),
                "a span must not end in whitespace: {text:?}"
            );
        }
    }
}
