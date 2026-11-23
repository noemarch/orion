# Issue 17: clean Windows install proof

Date: 2026-11-22. Machine: Windows (win32), Rust 1.100, Zig 0.17.1, `claude` on `PATH`.

## What this proves

A release package can be produced from the native binaries, verified, extracted
into a clean user-local directory, and launched as the native `orion-ui` app.
The app opens a real Workspace with one agent Pane.

## Scenario 1: package

Command used locally:

```powershell
pwsh -NoProfile -File scripts/package-release.ps1 \
  -Version v0.1.0 -Os windows-latest \
  -BinaryDir <binary-dir> -OutDir <qa-dir>\s1-package
```

Result: `orion-v0.1.0-windows-latest.zip` (37,029,371 bytes) and
`orion-v0.1.0-windows-latest.zip.sha256`. `tar -tf` lists `orion.exe`,
`orion-ui.exe`, and `orion-rt.exe` at the archive root. The SHA-256 from
`Get-FileHash` matches the sidecar.

The two PowerShell seams have failing-first tests that now pass:
`test-package.ps1` and `test-install.ps1` (exit 0, both report GREEN). The
install helper also rejects a tampered sidecar.

## Scenario 2: clean install and Pane

Commands:

```powershell
pwsh -NoProfile -File scripts/install-orion.ps1 \
  -ArchivePath <qa-dir>\s1-package\orion-v0.1.0-windows-latest.zip \
  -InstallDir <qa-dir>\s2-install\app
pwsh -NoProfile -File <qa-dir>\s2-install\clean-install-qa.ps1
```

Observed output:

```
Installed to: <qa-dir>\s2-install\app
APP_PID=15848 SESSION=qa-issue17-428fde98
WINDOW=Orion 1616x908
SCREENSHOT=screenshot.png
WORKSPACES=1 PANES=1
PANE id=94f5c2e3-3bba-477a-abe4-f262399b565f agent=claude cwd=C:\rust-projects\orion
S2_PASS=clean install launched native app and opened a real claude Pane
KILLED=15848,15444,15848
```

The screenshot is committed as
[`screenshots/orion-1280x800-issue17-clean-install.png`](screenshots/orion-1280x800-issue17-clean-install.png).
The QA harness used a fresh `ORION_DATA_DIR`, a unique `ORION_RUNTIME_SESSION`,
and a clean install directory. It proved the Pane through the Engine's own
`workspaces.json` session document, not through a UI text assertion. No `orion`
processes remained after teardown.

**Prerequisite:** opening an agent Pane needs an authenticated agent CLI on
`PATH`. This run used `claude`. Without an agent CLI the app still launches, but
no agent Pane can open.

## Scenario 3: regressions

- `cargo fmt --all --check`: exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `scripts/check_no_webview.ps1 -Binary <debug orion-ui>`: pass; no WebView
  markers and no forbidden root web-asset directory.
- `cargo test --workspace -j 1 --no-fail-fast` with `HOME` and `USERPROFILE`
  set: all unit, integration, and doc tests pass except one pre-existing
  timing assertion, `terminal_startup::the_terminal_startup_timeline_separates_the_agent_from_the_app`,
  which required the control Pane to show content within 200 ms and measured
  422.5 ms on this loaded host. It is a performance gate, not a functional
  assertion, and no test was changed or skipped.

## Scenario 4: documentation

- `AGENTS.md` no longer claims system tray integration; it states that tray,
  desktop notifications, updater, and single-instance are absent and points to
  the ADR.
- `docs/adr/desktop-shell-ownership.md` records full Tauri retirement and all
  four behavior decisions, including the per-session daemon lock.
- `docs/install.md` gives the versioned-package install method, checksum
  verification, the Zig and agent-CLI prerequisites, and the capability list.
- `docs/migration.md` no longer claims nonexistent GitHub-release installers.
- `PLAN.md`, `docs/roadmap.md`, `docs/orion-pure-rust-plan.md`,
  `docs/gpui-kit-adoption.md`, and the two `docs/superpowers/` files point to
  the current decision instead of the Tauri-era plan.

## Limits of this proof

The local package was built from the workspace's existing debug binaries. A
full `cargo build --release --workspace` did not finish on this host: the
compiler was killed three times by tool timeouts and once by host paging
pressure (`os error 1455`). The release build is exercised by
`.github/workflows/release.yml`, which now installs Zig, installs the Linux GUI
libraries, packages all three binaries with a checksum sidecar, and runs the
no-WebView gate before packaging. This proof therefore covers the package and
install mechanism end to end; the optimized release compile is left to CI.
