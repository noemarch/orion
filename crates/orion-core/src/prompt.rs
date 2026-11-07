//! Reading an agent's interactive prompt off the terminal grid.
//!
//! A CLI agent asks for permission by drawing a menu: a question line, numbered options and one
//! marked row. The chat lens shows that menu as buttons, and an answer is the same keystrokes a
//! user would type — never a second protocol the agent does not have. Detection is therefore pure
//! text over [`ScreenState::visible_text`](crate::screen::ScreenState::visible_text), and the
//! answer is validated against a fresh read, so a card left open over a screen that moved on
//! cannot answer the wrong question.

use serde_json::{json, Value};

/// One row of a detected menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuOption {
    pub number: usize,
    pub label: String,
    pub description: Option<String>,
    pub selected: bool,
}

/// A menu read from the grid, with the fingerprint a card answers by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPrompt {
    /// Stable while the menu stays the same; a different screen is a different id.
    pub id: String,
    pub agent: String,
    pub kind: &'static str,
    pub title: String,
    pub question: String,
    pub body: Option<String>,
    pub options: Vec<MenuOption>,
    pub custom_option_index: Option<usize>,
    pub queued: Option<&'static str>,
    pub row_count: usize,
    pub selected_row: Option<usize>,
}

impl DetectedPrompt {
    pub fn selected(&self) -> Option<usize> {
        self.options.iter().position(|option| option.selected)
    }

    /// The card the client draws (`InteractivePrompt` in `shared/protocol.ts`).
    pub fn to_wire(&self) -> Value {
        let options = self
            .options
            .iter()
            .map(|option| {
                json!({
                    "label": option.label,
                    "description": option.description,
                })
            })
            .collect::<Vec<_>>();
        let mut wire = json!({
            "id": self.id,
            "agent": self.agent,
            "kind": self.kind,
            "title": self.title,
            "question": self.question,
            "body": self.body,
            "options": options,
            "multi_select": false,
            "custom_option_index": self.custom_option_index,
        });
        if let Some(queued) = self.queued {
            wire["queued"] = json!(queued);
        }
        wire
    }

    /// The keystrokes that move the menu's cursor onto `target` and choose it.
    ///
    /// Claude wraps at both ends, so a move may go either way; the shorter way is taken. `None`
    /// when `target` names no row.
    pub fn answer_keys(&self, target: usize) -> Option<String> {
        if target >= self.options.len() {
            return None;
        }
        let current = self.selected_row.or_else(|| self.selected())?;
        let count = self.row_count;
        let (down, up) = if self.agent == "codex" {
            // Codex lists do not wrap.
            let down = target.saturating_sub(current);
            let up = current.saturating_sub(target);
            (down, up)
        } else {
            let down = (target + count - current) % count;
            let up = (current + count - target) % count;
            (down, up)
        };
        let mut keys = String::new();
        if self.agent == "codex" {
            if target >= current {
                keys.push_str(&"\x1b[B".repeat(target - current));
            } else {
                keys.push_str(&"\x1b[A".repeat(current - target));
            }
        } else if down <= up {
            keys.push_str(&"\x1b[B".repeat(down));
        } else {
            keys.push_str(&"\x1b[A".repeat(up));
        }
        keys.push('\r');
        Some(keys)
    }

    /// The keystrokes that move onto the custom row, type the answer, and submit it.
    pub fn answer_custom_keys(&self, text: &str) -> Option<String> {
        let custom = self.custom_option_index?;
        if custom >= self.row_count {
            return None;
        }
        let current = self.selected_row.or_else(|| self.selected())?;
        let count = self.row_count;
        let (down, up) = if self.agent == "codex" {
            let down = custom.saturating_sub(current);
            let up = current.saturating_sub(custom);
            (down, up)
        } else {
            let down = (custom + count - current) % count;
            let up = (current + count - custom) % count;
            (down, up)
        };
        let mut keys = String::new();
        if self.agent == "codex" {
            if custom >= current {
                keys.push_str(&"\x1b[B".repeat(custom - current));
            } else {
                keys.push_str(&"\x1b[A".repeat(current - custom));
            }
        } else if down <= up {
            keys.push_str(&"\x1b[B".repeat(down));
        } else {
            keys.push_str(&"\x1b[A".repeat(up));
        }
        keys.push_str(text);
        keys.push('\r');
        Some(keys)
    }
}

/// Reads a numbered menu from visible screen text.
///
/// The last run that counts up from 1 wins: a message still on screen above the menu can hold a
/// numbered line of its own, and the menu is the bottom-most one. Rows must belong to the same
/// column (a numbered line indented further is an option's description), and exactly one row must
/// be marked — a menu with no cursor is not something a keystroke can answer.
pub fn detect_menu(text: &str, agent: &str) -> Option<DetectedPrompt> {
    let lines: Vec<&str> = text.lines().collect();
    let rows = numbered_rows(&lines);
    if rows.len() < 2 {
        return None;
    }

    // The bottom-most run counting 1..n in one column.
    let anchor = rows
        .iter()
        .rev()
        .find(|row| row.selected)
        .or_else(|| rows.last())?;
    let same_column: Vec<&NumberedRow> = rows
        .iter()
        .filter(|row| row.column == anchor.column)
        .collect();
    let start = same_column.iter().rposition(|row| row.number == 1)?;
    let run = &same_column[start..];
    if run.len() < 2
        || !run
            .iter()
            .enumerate()
            .all(|(index, row)| row.number == index + 1)
    {
        return None;
    }
    let selected = run.iter().filter(|row| row.selected).count();
    if selected != 1 {
        return None;
    }

    let first = run.first()?.line;
    let last = run.last()?.line;
    // Claude ends an approval question with `?` and a choice menu's heading with `:`; both are
    // answered by moving the cursor. A message's numbered list has no marked row, so the cursor
    // is what separates a menu from prose, not the punctuation. Codex names the same shape a
    // question.
    let (question_index, raw_kind) = (0..first)
        .rev()
        .find_map(|index| prompt_kind(lines[index]).map(|kind| (index, kind)))?;
    let kind = if agent == "codex" && raw_kind == "approval" {
        "question"
    } else {
        raw_kind
    };
    let question = clean(lines[question_index]);
    let (mut title, body) = panel(&lines, question_index, kind);
    if agent == "codex" {
        title = codex_title(&lines, question_index);
    }

    let mut options = run
        .iter()
        .map(|row| MenuOption {
            number: row.number,
            label: row.label.clone(),
            description: description(&lines, row.line, last),
            selected: row.selected,
        })
        .collect::<Vec<_>>();

    // Codex ends a question menu with an "Other"/"None of the above" row that takes a typed
    // answer: the card offers the real options and names that row as the custom option.
    let selected_row = run.iter().position(|row| row.selected);
    let row_count = run.len();
    let mut custom_option_index = None;
    if agent == "codex" {
        if let Some(last_option) = options.last() {
            let label = last_option.label.trim().to_lowercase();
            if label.starts_with("other") || label.starts_with("none of the above") {
                options.pop();
                if !options.is_empty() {
                    custom_option_index = Some(options.len());
                }
            }
        }
    }

    let id = fingerprint(agent, &question, &options);
    let queued = if agent == "codex"
        && lines
            .iter()
            .any(|line| clean(line).contains("Queued follow-up inputs"))
    {
        Some("open")
    } else {
        None
    };
    Some(DetectedPrompt {
        id,
        agent: agent.to_string(),
        kind,
        title,
        question,
        body,
        options,
        custom_option_index,
        queued,
        row_count,
        selected_row,
    })
}

fn codex_title(lines: &[&str], question_index: usize) -> String {
    if question_index > 0 {
        let above = clean(lines[question_index - 1]);
        if let Some((current, total)) = above.split_once(" of ") {
            if current.trim().chars().all(|c| c.is_ascii_digit())
                && total.trim().chars().all(|c| c.is_ascii_digit())
            {
                return format!("Question {} of {}", current.trim(), total.trim());
            }
        }
    }
    "Question".to_string()
}

/// Detects the prompt Claude Code is showing, if any. Claude is the only agent the walking
/// skeleton answers; other agents keep the terminal as their prompt surface until their own
/// readers land.
pub fn detect_claude(text: &str) -> Option<DetectedPrompt> {
    detect_menu(text, "claude")
}

/// Detects the prompt Codex is showing, if any: an open question menu on the grid.
pub fn detect_codex(text: &str) -> Option<DetectedPrompt> {
    detect_menu(text, "codex")
}

/// A question Codex asked with `request_user_input_async` and has not answered yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedQuestion {
    pub key: String,
    pub title: String,
    pub options: Vec<String>,
}

/// How many questions wait in Codex's collapsed queue at the bottom of the screen.
pub fn codex_queued_count(screen: &str) -> usize {
    let lines: Vec<String> = screen
        .lines()
        .map(clean)
        .filter(|line| !line.is_empty())
        .collect();
    let Some(header) = lines
        .iter()
        .rposition(|line| line.contains("Queued follow-up inputs"))
    else {
        return 0;
    };
    if lines.len() - header > 16 {
        return 0;
    }
    if lines[header..].iter().any(|line| {
        line.starts_with("↳") || line.to_lowercase().contains("messages to be submitted")
    }) {
        return 0;
    }
    for (index, line) in lines.iter().enumerate().skip(header + 1).take(7) {
        let Some(rest) = line.strip_prefix('?') else {
            continue;
        };
        let rest = rest.trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() || !rest[digits.len()..].trim_start().starts_with("question") {
            continue;
        }
        if !lines
            .get(index + 1)
            .is_some_and(|next| next.to_lowercase().contains("to answer"))
        {
            continue;
        }
        if let Some(after) = lines.get(index + 2) {
            if !after.starts_with("›") {
                continue;
            }
        }
        return digits.parse().unwrap_or(0);
    }
    0
}

/// The card for a collapsed queue: the screen shows only a count, so the first question comes
/// from the rollout's newest unanswered ones.
pub fn codex_queued_prompt(screen: &str, unanswered: &[QueuedQuestion]) -> Option<DetectedPrompt> {
    let count = codex_queued_count(screen);
    if count == 0 || unanswered.len() < count {
        return None;
    }
    let waiting = &unanswered[unanswered.len() - count..];
    let first = &waiting[0];
    if first.title.trim().is_empty() {
        return None;
    }
    let question = normalize_text(&first.title);
    let options = first
        .options
        .iter()
        .enumerate()
        .map(|(index, label)| MenuOption {
            number: index + 1,
            label: label.clone(),
            description: None,
            selected: index == 0,
        })
        .collect::<Vec<_>>();
    let title = if count > 1 {
        format!("Question 1 of {count}")
    } else {
        "Question".to_string()
    };
    let id = fingerprint("codex", &question, &options);
    Some(DetectedPrompt {
        id,
        agent: "codex".to_string(),
        kind: "question",
        title,
        question,
        body: None,
        options: options.clone(),
        custom_option_index: Some(options.len()),
        queued: Some("collapsed"),
        row_count: options.len() + 1,
        selected_row: Some(0),
    })
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the agent is drawing a prompt that only a user can answer. This is what puts the pane
/// in `blocked` and raises the approval card.
pub fn is_claude_blocked(text: &str) -> bool {
    detect_claude(text).is_some()
}

/// Whether Codex is drawing a prompt that only a user can answer.
pub fn is_codex_blocked(text: &str) -> bool {
    detect_codex(text).is_some()
}

/// Whether Claude is between turns rather than working. The TUI prints an interrupt hint while a
/// turn runs and returns the input box when it is done.
pub fn claude_is_working(text: &str) -> bool {
    let tail = text
        .lines()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    tail.contains("esc to interrupt")
        || tail.contains("esc to stop")
        || tail.contains("ctrl+b to run in background")
}

struct NumberedRow {
    number: usize,
    label: String,
    selected: bool,
    column: usize,
    line: usize,
}

fn numbered_rows(lines: &[&str]) -> Vec<NumberedRow> {
    let mut rows = Vec::new();
    for (line_index, raw) in lines.iter().enumerate() {
        let text = clean(raw);
        if text.is_empty() {
            continue;
        }
        if text.to_lowercase().contains("esc to cancel") {
            continue;
        }
        let mut rest = text.as_str();
        let mut selected = false;
        if let Some(after) = rest.strip_prefix('❯').or_else(|| rest.strip_prefix('›')) {
            selected = true;
            rest = after.trim_start();
        }
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            continue;
        }
        let (number, after_number) = rest.split_at(digits);
        let Some(after_dot) = after_number.strip_prefix('.') else {
            continue;
        };
        let label = after_dot.trim_start();
        if label.is_empty() {
            continue;
        }
        let Ok(number) = number.parse::<usize>() else {
            continue;
        };
        // The column of the first digit, counted in characters: the marker before it may be
        // multibyte. Rows of the same menu share this column.
        let column = raw.chars().take_while(|c| !c.is_ascii_digit()).count();
        rows.push(NumberedRow {
            number,
            label: label.to_string(),
            selected,
            column,
            line: line_index,
        });
    }
    rows
}

/// The panel above a menu: a known heading names the card, and the text under it is the body.
fn panel(lines: &[&str], question_index: usize, kind: &str) -> (String, Option<String>) {
    const HEADINGS: [&str; 6] = [
        "Bash command",
        "Edit file",
        "Write file",
        "Read file",
        "Web fetch",
        "MCP tool",
    ];
    let mut title = String::new();
    let mut body = Vec::new();
    let floor = question_index.saturating_sub(14);
    for index in (floor..question_index).rev() {
        let text = clean(lines[index]);
        if is_divider(&text) {
            break;
        }
        if title.is_empty() {
            if let Some(heading) = HEADINGS
                .iter()
                .find(|heading| text.eq_ignore_ascii_case(heading))
            {
                title = (*heading).to_string();
                continue;
            }
        }
        if !text.is_empty() {
            body.push(text);
        }
    }
    body.reverse();
    if title.is_empty() {
        title = if kind == "menu" { "Menu" } else { "Approval" }.to_string();
    }
    let body = body.join("\n");
    let body = if body.trim().is_empty() {
        None
    } else {
        Some(body)
    };
    (title, body)
}

/// A wrapped description under an option, up to the next row or the hint.
fn description(lines: &[&str], line: usize, last_row: usize) -> Option<String> {
    let mut text = Vec::new();
    for raw in lines.iter().take(last_row + 6).skip(line + 1) {
        let value = clean(raw);
        if value.is_empty() || is_divider(&value) || is_question(&value) {
            break;
        }
        if value.to_lowercase().contains("esc to cancel") {
            break;
        }
        if numbered_prefix(&value).is_some() {
            break;
        }
        if raw.starts_with("  ") {
            text.push(value);
        } else {
            break;
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text.join(" "))
    }
}

fn numbered_prefix(text: &str) -> Option<usize> {
    let rest = text.trim_start_matches(['❯', '›']).trim_start();
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && rest[digits..].starts_with('.') {
        Some(digits)
    } else {
        None
    }
}

/// A line that asks for a choice: a question (`?`) or a menu heading (`:`). Claude draws both
/// the same way — a heading, numbered options and one marked row.
fn prompt_kind(text: &str) -> Option<&'static str> {
    let text = clean(text);
    match text.chars().last()? {
        '?' => Some("approval"),
        ':' if text.chars().count() > 1 => Some("menu"),
        _ => None,
    }
}

fn is_question(text: &str) -> bool {
    prompt_kind(text) == Some("approval")
}

fn is_divider(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| matches!(c, '─' | '━' | '═' | '-' | '=' | '╭' | '╮' | '╰' | '╯'))
}

fn clean(text: &str) -> String {
    text.trim().to_string()
}

/// FNV-1a over the menu's identity. A 12-hex-digit id is plenty to tell two menus apart and keeps
/// the card's id short in a URL.
fn fingerprint(agent: &str, question: &str, options: &[MenuOption]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    };
    eat(agent.as_bytes());
    eat(b"\0");
    eat(question.as_bytes());
    for option in options {
        eat(b"\0");
        eat(option.label.as_bytes());
    }
    format!("{hash:012x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPROVAL: &str = "\
● Deleting the junk directory
  ⎿  $ rm -rf junk
────────────────────────────────────────
 Bash command
 Tip: auto mode handles these prompts for you — choose \"switch to auto mode\" below
   rm -rf junk
   Delete the junk directory
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /tmp/prompt-lab/junk from this project
   3. Yes, and switch to auto mode · auto mode handles these prompts for you
   4. No
 Esc to cancel · Tab to amend
";

    /// Claude Code's first screen, read off a live ConPTY grid on Windows.
    const THEME: &str = "\
╭──────────────────────────╮
│ ✻ Welcome to Claude Code │
╰──────────────────────────╯

 Let's get started.

 Choose the text style that looks best with your terminal:
 To change this later, run /theme

 ❯ 1. Dark mode ✔
   2. Light mode
   3. Dark mode (colorblind-friendly)
   4. Light mode (colorblind-friendly)
   5. Dark mode (ANSI colors only)
   6. Light mode (ANSI colors only)


 Preview
 ╭────────────────────────────────────────────╮
 │   1   function greet() {                   │
 │   2 -    console.log(\"Hello, World!\");      │
 │   2 +    console.log(\"Hello, Claude!\");     │
 │   3   }                                    │
 ╰────────────────────────────────────────────╯
";

    #[test]
    fn reads_a_claude_approval_menu() {
        let prompt = detect_claude(APPROVAL).expect("a menu");
        assert_eq!(prompt.kind, "approval");
        assert_eq!(prompt.title, "Bash command");
        assert_eq!(prompt.question, "Do you want to proceed?");
        assert_eq!(prompt.options.len(), 4);
        assert_eq!(prompt.selected(), Some(0));
        assert!(prompt.body.unwrap().contains("rm -rf junk"));
    }

    #[test]
    fn a_menu_is_not_a_prompt_while_the_agent_works() {
        assert!(detect_claude("✶ Cogitating… (esc to interrupt)").is_none());
        assert!(claude_is_working("✶ Cogitating… (esc to interrupt)"));
    }

    #[test]
    fn reads_claude_startup_menu_from_a_real_screen() {
        let prompt = detect_claude(THEME).expect("the startup menu");
        assert_eq!(prompt.kind, "menu");
        assert_eq!(prompt.title, "Menu");
        assert_eq!(
            prompt.question,
            "Choose the text style that looks best with your terminal:"
        );
        assert_eq!(prompt.options.len(), 6);
        assert_eq!(prompt.selected(), Some(0));
        assert_eq!(prompt.options[5].label, "Light mode (ANSI colors only)");
        // The preview box below the menu has numbered rows of its own; none of them join it.
        assert!(prompt
            .options
            .iter()
            .all(|option| !option.label.contains("function")));
    }

    #[test]
    fn answering_moves_the_cursor_the_short_way_and_submits() {
        let prompt = detect_claude(APPROVAL).unwrap();
        assert_eq!(prompt.answer_keys(0).unwrap(), "\r");
        assert_eq!(prompt.answer_keys(1).unwrap(), "\x1b[B\r");
        // four rows, so one up wraps onto the last
        assert_eq!(prompt.answer_keys(3).unwrap(), "\x1b[A\r");
        assert!(prompt.answer_keys(9).is_none());
    }

    #[test]
    fn a_menu_missing_its_cursor_is_not_answerable() {
        let text = "Do you want to proceed?\n 1. Yes\n 2. No\n Esc to cancel";
        assert!(detect_claude(text).is_none());
    }

    const CODEX_OPEN: &str = "\
• WAITING
• Queued follow-up inputs
  1 of 2
  Which split?
  › 1. train
    2. test
    3. Other
  enter submit   ctrl+] skip   alt+↓ main prompt   alt+↑ next question
";

    #[test]
    fn reads_an_open_codex_async_question() {
        let prompt = detect_codex(CODEX_OPEN).expect("the open question");
        assert_eq!(prompt.agent, "codex");
        assert_eq!(prompt.kind, "question");
        assert_eq!(prompt.title, "Question 1 of 2");
        assert_eq!(prompt.question, "Which split?");
        assert_eq!(prompt.options.len(), 2);
        assert_eq!(prompt.options[0].label, "train");
        assert_eq!(prompt.options[1].label, "test");
        assert_eq!(prompt.custom_option_index, Some(2));
        assert_eq!(prompt.queued, Some("open"));
        assert_eq!(prompt.answer_keys(1).unwrap(), "\x1b[B\r");
        assert_eq!(
            prompt.answer_custom_keys("Q255 only").unwrap(),
            "\x1b[B\x1b[BQ255 only\r"
        );
    }

    const CODEX_COLLAPSED: &str = "\
• WAITING
• Queued follow-up inputs
  ? 2 questions · 8s
    alt+↑ to answer
› Ask Codex to do anything
  GPT-6-Sol xhigh · ~/lab · Context 97% left
";

    #[test]
    fn reads_a_collapsed_codex_queue_from_the_rollout() {
        assert_eq!(codex_queued_count(CODEX_COLLAPSED), 2);
        let unanswered = vec![
            QueuedQuestion {
                key: "call_c:0".into(),
                title: "Which dataset?".into(),
                options: vec!["LM-O".into()],
            },
            QueuedQuestion {
                key: "call_c:1".into(),
                title: "Any notes?".into(),
                options: Vec::new(),
            },
        ];
        let prompt = codex_queued_prompt(CODEX_COLLAPSED, &unanswered).expect("the collapsed card");
        assert_eq!(prompt.agent, "codex");
        assert_eq!(prompt.kind, "question");
        assert_eq!(prompt.title, "Question 1 of 2");
        assert_eq!(prompt.question, "Which dataset?");
        assert_eq!(prompt.options.len(), 1);
        assert_eq!(prompt.queued, Some("collapsed"));
    }

    #[test]
    fn a_narrow_wrapped_option_stays_one_label() {
        let text = "\
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /tmp/a
      very long path that wrapped
   3. No
 Esc to cancel
";
        let prompt = detect_claude(text).unwrap();
        assert_eq!(prompt.options.len(), 3);
        assert_eq!(
            prompt.options[1].label,
            "Yes, and always allow access to /tmp/a"
        );
        assert_eq!(
            prompt.options[1].description.as_deref(),
            Some("very long path that wrapped")
        );
    }

    #[test]
    fn the_fingerprint_follows_the_question_and_its_options() {
        let first = detect_claude(APPROVAL).unwrap();
        let other = APPROVAL.replace("Do you want to proceed?", "Do you want to delete it?");
        let second = detect_claude(&other).unwrap();
        assert_ne!(first.id, second.id, "another question is another prompt");
        // moving the cursor with the arrow keys does not stale the card: the answer is computed
        // from the row selected when the user presses, not the one the card was drawn with
        let moved = APPROVAL
            .replace("❯ 1. Yes", "  1. Yes")
            .replace("   4. No", " ❯ 4. No");
        assert_eq!(first.id, detect_claude(&moved).unwrap().id);
    }
}
