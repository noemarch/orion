# Chat Lens: tool cards, diffs, and file affordances (#10)

## Goal
The Chat Lens shows what the Agent did, not just what it said: tool calls
become collapsible rows with name, status, and summary; their input and
output render in place as monospace; an edit diff shows added and deleted
lines; file paths are actionable; a failed tool call stays visible; and
oversized output points at the Terminal Lens.

## Seams under test
The issue's acceptance criteria name the seams; the tests live there.

1. `tool_card` (pure): the part-to-card mapping, the diff parser, the file
   path finder, and the truncation bound.
2. `chat_model` (pure): the per-row open state, shared by thinking and tool
   rows.
3. `ChatView` (GPUI render): the tool row, its diff lines, the file path
   button, and the Terminal path.

## Tasks

1. **TDD: the part-to-card mapping** — `tool_card::part_card` turns a turn
   part into what the surface draws: text, thinking, a tool card, or a diff.
2. **TDD: the diff parser** — `tool_card::diff_lines` reads a unified diff
   into added, deleted, and context lines.
3. **TDD: file paths** — `tool_card::file_paths` finds the paths in a tool's
   text.
4. **TDD: the truncation bound** — the output cut at the bound, and the
   marker that says where the rest is.
5. **Tool rows** — collapsible, per-row state, name + status + summary; the
   input and output in a monospace block.
6. **Diff rendering** — added lines in `--syntax-inserted`, deleted lines in
   `--syntax-deleted`, with their context.
7. **File affordances** — a path in a tool card is a button that opens the
   file in the Changes lens.
8. **The Terminal path** — "View full in Terminal" switches the lens.
9. **Tests + verification** — `cargo test --workspace`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo fmt --all --check`, then `code-review`.
