# Issue #16 — visual checklist per lens

Method: each lens row was checked against the rendered app at 1280x800 (dark, light, compact where relevant) and against the source path that produces the state. "Code" means the state exists in the implementation and is covered by a named test; "Screenshot" means the state is visible in one of the captured artifacts. Keyboard and 44px checks are code/geometry assertions where a static capture cannot prove them.

| Lens / surface | Icon + mark set | Hover | Focus ring | Selected | Disabled | Pressed | Keyboard path | 44px targets | Evidence |
|---|---|---|---|---|---|---|---|---|---|
| Sidebar | Lucide (Menu, PanelLeft, Search, Settings, Plus, SquareTerminal, ChevronRight, LayoutDashboard); first-letter avatar mark, token sizes | Code | Code + screenshot | Code (row fill) | N/A (no disabled rows) | Code | Code + screenshot | Code | `sidebar.rs`, `theme.rs` tokens, dark screenshot |
| Tab strip | Lucide MessageCircle / SquareTerminal / Pencil / Columns2 / X / Plus | Code | Code | Code (TabBar selected) | N/A | Code | Code (suffix buttons tab stops) | Code | `app.rs:1546-1716` |
| Lens controls | Lucide MessageCircle / SquareTerminal / FileDiff + ZoomIn / X | Code | Code | Code (segmented) | N/A | Code | Code (arrow-key cycle; tab stops added) | Code | `app.rs:1453-1542`, `shell_model.rs` |
| Chat | Lucide carets, content glyphs kept semantic (bullet, `$`, diff signs) | Code | Code | Code (tool rows) | Code | Code | Code | Code | `chat.rs`, chat tests |
| Terminal | Lucide arrows / X / Kbd keycaps; block sprites for grid art | Code | Code + screenshot | Code | Code | Code | Code + screenshot | Code | `terminal.rs`, `terminal_grid.rs`, `sprites.rs` |
| Composer | Lucide ArrowUp / Square | Code | Code (new focus ring) | N/A | Code (empty/pending/failed) | Code | Code + test (`the_composer_input_is_a_keyboard_tab_stop`) | Code | `composer.rs:653-698`, `tests` |
| Prompt card | Lucide header glyph; numbered options | Code | Code (new ring + border) | Code | Code (pending/sent) | Code | Code + number keys | Code (options/toggle/send) | `prompt_card.rs:208-356` |
| File tree / reader | Lucide Folder / File / ChevronDown / ChevronRight; status letters from shared marks | Code | Code | Code (row fill) | N/A | Code | Code + test (`arrow_keys_walk_the_rows...`) | Code (splitter hit box 44px) | `files_view.rs`, `files_entities.rs`, tests |
| Diff / changes | Lucide ArrowRight / FileText / Folder / File; status letters token-coloured | Code | Code + screenshot | Code | Code (disabled scope chip) | Code | Code + arrow-key scope test | Code (44px splitter, retry) | `diff_view.rs:851-1221`, tests |
| Review column | Lucide FileText / ChevronDown / RefreshCw / Settings; inert controls now explicitly disabled | Code | Code (new focus root) | Code | Code (scope + settings disabled) | Code | Code (focus root + tab stops) | Code | `review_pane.rs:330-390` |
| Settings | Lucide X on close; Kbd key labels | Code | Code + screenshot | Code (radios/switches) | N/A | Code | Code (focus trap, Escape, restored focus) | Code (close + Done 44px) | `settings.rs:624-719`, tests |
| New workspace / shell picker | Lucide ChevronRight / X | Code | Code | Code (radio) | Code | Code | Code | Code | `new_workspace.rs`, `shell_picker.rs` |

## Cross-cutting checks

- All eight primary surfaces are reachable by keyboard: global Tab/Shift+Tab comes from gpui-base root; raw focusable views were converted to `cx.focus_handle().tab_stop(true)`; kit Buttons are tab stops by default. Tab/TabBar scope stripes use explicit arrow-key handlers because gpui-base Tabs have no keyboard focus.
- Focus ring: shared token-driven ring via `focus_ring_style`, visible in dark/light; the composer and prompt card previously had no ring and now do.
- Touch targets: a fixed `TOUCH_TARGET_PX = 44.0` (not density-scaled) is asserted by `theme.rs` tests and at the file/diff splitter hit boxes; compact density shrinks typography and spacing but never the touch minimum.
- Icon family: full Lucide catalog via `gpui_kit::assets::IconName`; the legacy subset enum is no longer mixed into new call sites. Agent marks are one avatar component with token sizes.
- Status colours: RUN blue, INPUT amber, DONE green, ERROR red; rose reserved for focus/selection. No status colour is rose-only, and state is not carried by colour alone (`session_status.rs` badge words).

## Gate results recorded

- `cargo fmt --all --check`: pass after normalization.
- `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo fmt --all --check`: pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo test --workspace`: **exit 0 in a single run** (all crates, integration tests and doc-tests), and all four GitHub Actions checks green on Windows, macOS and Ubuntu (run `45020985475`). Getting there required fixing pre-existing CI gaps: Zig 0.17.1 was not installed, the Linux GUI system libraries were missing, the artifact scan matched migration docs, a nonblocking-socket race broke Unix daemon handshakes, and several tests assumed a `claude` CLI on the runner. CLI-dependent tests now skip when the CLI is absent, following the repo's existing smoke-test pattern; assertions were not weakened.
- Headless `repaint_storm` frame timings are a regression trace; the 60fps gate uses live app measurements instead (see `docs/perf/2026-11-22-issue16-frame-gate.md`).
- `scripts/check_no_webview.ps1`: pass against the workspace-built binary.
