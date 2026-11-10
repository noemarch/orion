# Chat Lens: live Transcript rendering (#9)

## Goal
The Chat Lens renders the Engine's Transcript as a conversation: user and agent
turns with role and timestamp, Markdown prose in the Quiet Slate type scale,
collapsible thinking rows, an explicit loading / empty / failed state, and a
Retry that recovers without a restart. The terminal grid never becomes a turn.

## Seams under test
The issue's acceptance criteria name the seams; the tests live there.

1. `chat_model` (pure): load phase, thinking-row state, follow-the-bottom rule,
   timestamp formatting.
2. `markdown` (pure): markdown source → block kinds, inline runs, link targets.
3. `ChatView` (GPUI render): the chat surface, the turn rows, and the error
   state are on screen.

## Tasks

1. **TDD: load phase** — `chat_model::ChatPhase` (`Loading`, `Ready`, `Empty`,
   `Failed`) driven by a fetch outcome; a failed fetch keeps a plain-language
   message and a Retry.
2. **TDD: thinking rows** — per (turn, part) open/closed state, preserved when
   the transcript grows.
3. **TDD: follow the bottom** — the view follows new turns only while the
   reader is at the bottom; scrolling up pins the view, scrolling back to the
   bottom resumes following.
4. **TDD: timestamps** — the wire's RFC 3339 stamp becomes a short local time;
   a missing stamp reads as no stamp.
5. **Markdown type scale** — headings, lists, inline code, fenced code, and
   links render with the Quiet Slate scale; links carry their target.
6. **Chat surface** — role and timestamp per turn, tool cards, thinking rows.
7. **States** — skeleton while loading, an empty state for a transcript with no
   turns, a plain-language failure with a Retry control.
8. **Tests + verification** — `cargo test --workspace`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo fmt --all --check`, then `code-review`.
