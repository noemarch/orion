# Issue #16 — visual parity comparison (1280x800)

Captured from the workspace build `G:\Temp\rust\target\debug\orion-ui.exe` on 2026-11-22, driven through `scripts/capture_issue16.ps1`. Each capture is the real OS window surface at exactly 1280x800, with an isolated `ORION_DATA_DIR` so the user's own settings were never modified.

## Artifacts

| Mode | Screenshot | Pixels | Distinct sampled colours |
|---|---|---|---|
| Dark, comfortable | `screenshots/orion-1280x800-dark-comfortable.png` | 1280x800 | 263 |
| Light, comfortable | `screenshots/orion-1280x800-light-comfortable.png` | 1280x800 | 253 |
| Dark, compact | `screenshots/orion-1280x800-dark-compact.png` | 1280x800 | 469 |

Counts are the distinct sampled colours from the full-resolution capture pipeline (step-6 sampling in `scripts/capture_issue16.ps1`), not the coarser 10px-grid verification pass.

Capture method: `SetForegroundWindow` + `Graphics.CopyFromScreen` on the window rect, with `GPUI_DISABLE_DIRECT_COMPOSITION=1`. `PrintWindow` was rejected for the final artifacts because it returned a blank/near-uniform surface for the GPUI window; it is retained in the script only as a diagnostic channel. Every PNG was verified at 1280x800 with non-uniform content before being accepted.

## Reference packet

No running web client binary was available. The comparison target is therefore the static web reference and its written contract:

- `docs/herd-ui-reference.html` — static dark hierarchy mock (300px sidebar, titlebar, conversation, review/diff column, prompt block, composer).
- `docs/superpowers/specs/2026-11-10-orbit-ui-port-design.md` — shell IA, per-lens parity, visual acceptance at 1280x800.
- `DESIGN.md` — web token table, component hierarchy and compact overrides.

This is an honest substitution for "side-by-side with the web client": native capture beside a static reference + written hierarchy, with differences listed below.

## Hierarchy comparison

| Surface | Reference (web mock / spec) | Native at 1280x800 | Verdict |
|---|---|---|---|
| Shell | Sidebar + main column, header carries brand/workspace/context/actions (`spec:35-44`) | `app.rs` header (40px) + `#sidebar` + `#main-column`; three-column layout available when review is open | Match in hierarchy; size difference listed |
| Sidebar | One row grid (mark, title/context, trailing status); Workspaces + Agents as independent sections (`spec:41`, `DESIGN.md:994-1162`) | `sidebar.rs`: titlebar, search, sessions split into workspaces/agents/terminals, status section, bottom bar | Match |
| Tab strip | Horizontal, scrollable, selected state and menus (`spec:42`, `DESIGN.md:1190-1256`) | `TabBar("tab-strip")` with per-tab rename/split/close and new-tab action | Match |
| Lens controls | Compact segmented switcher, no full-width rose bar (`spec:46-52`) | `#lens-toggle` segmented Chat/Terminal/Changes + Zoom/Close, with Lucide glyphs now | Match |
| Composer | One surface, input card, model/attach/send actions, inviting empty state (`DESIGN.md:1642-1821`) | `#chat-composer` with Lucide `ArrowUp`/`Square`, focus ring and keyboard tab stop | Match in hierarchy |
| Prompt card | Docked above composer, flat option rows, custom answer, error line (`DESIGN.md:1575-1640`) | `#prompt-card` with numbered options, disabled states, keyboard number keys | Match in hierarchy |
| File tree / reader | Tree + reader split, folder/file glyphs (`DESIGN.md:1274-1288`, `:1467`) | `#file-tree` + `#files-splitter` + `#files-right`, Lucide folder/file and chevrons | Match in hierarchy; glyphs now from one set |
| Diff / changes | Changed-file list + diff split, scope tabs, status marks (`DESIGN.md:1274-1337`) | `#changes-view` with file list, splitter, diff, Lucide rename/status marks | Match in hierarchy |
| Settings | Focus-trapped modal, Appearance/Density/Interface size/Key bar/Alerts/Terminal/About (`spec:51`) | `#settings-panel` with those groups, focus trap, Escape, 44px controls | Match |
| Terminal lens | Live grid, key bar, find, status (`spec:49`) | Terminal grid + key bar + find + restart, block-element sprites painted as snapped quads | Match; terminal-specific accepted difference listed |

## Accepted differences

1. **Sidebar width**: the plan sketch used 280px (`docs/superpowers/plans/2026-11-10-orbit-ui-port.md:115`) while the implementation and `DESIGN.md:679` use 264px. The native value follows the design token; the plan sketch is stale.
2. **Header height**: `shell_model.rs:16` and `DESIGN.md:677` specify 46px, but the rendered header strip is 40px (`app.rs:1173`). The chrome functions correctly and all targets remain 44px; the 6px difference is accepted rather than churning layout at this gate.
3. **Palette source**: `DESIGN.md` documents `#0d1117`-family tokens, while shell chrome paints the additive "restyle" palette (`theme.rs:233-275`, accessors `:621-720`) with ground `#171a21`. Both are token-driven and theme-consistent; the difference is deliberate.
4. **Three-column at 1280**: with the review column open, 1280px is the clamp point (300 + 670 + 320 = 1290 > 1280), so the native app can show either the two-column shell or the clamped three-column shell at this width. Each screenshot pins `review_open = false` for a stable comparison.
5. **No live web side-by-side**: substitution by static mock + spec, as described above.
6. **Terminal block glyphs**: block elements are painted as device-pixel-snapped quads (`sprites.rs`) instead of text glyphs, which removes seams in agent-drawn frames but changes sub-pixel antialiasing versus the web renderer.
