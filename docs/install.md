# Installing Orion

Orion is a local-first agent orchestrator: a native GPUI desktop app and a
headless `orion serve` binary that exposes the same runtime over HTTP/WebSocket.

## Prerequisites

- Rust toolchain (stable) and `cargo`, if you build from source.
- Zig 0.17.x, if you build from source. The vendored `libghostty-vt` needs it.
- The agent CLIs you want to drive, already authenticated on the host:
  `claude`, `codex`, `opencode`, `cursor`, `grok`, `omp`, `omo`, `pi`, `agy`,
  `hermes`, `devin`. Orion discovers them on `PATH`. An agent pane needs one of
  them; without one the app opens but cannot start an agent. The default agent
  for a new workspace is `claude`.

## Install a release package on Windows

Each GitHub release publishes `orion-<version>-<target>.zip` and a `.sha256`
sidecar. The archive holds `orion-ui.exe`, `orion.exe`, and `orion-rt.exe`. Keep
the three binaries together: the shell starts `orion` from beside its own
executable.

1. Download `orion-v0.1.0-windows-latest.zip` and its `.sha256` sidecar from the
   release page.
2. Check the hash:

   ```powershell
   Get-FileHash .\orion-v0.1.0-windows-latest.zip -Algorithm SHA256
   Get-Content .\orion-v0.1.0-windows-latest.zip.sha256
   ```

   The two hashes must match.
3. Install into a user-local directory:

   ```powershell
   pwsh .\scripts\install-orion.ps1 -ArchivePath .\orion-v0.1.0-windows-latest.zip -InstallDir "$env:LOCALAPPDATA\Orion\bin"
   ```

   The script checks the hash again and extracts the binaries. Run it again to
   update an existing install in place. No administrator rights are needed.
4. Start the app:

   ```powershell
   & "$env:LOCALAPPDATA\Orion\bin\orion-ui.exe"
   ```

   Pin `orion-ui.exe` to Start if you want a shortcut. The shell has no tray
   icon.

## Install a release package on macOS or Linux

Download the matching `orion-<version>-macos-latest.zip` or
`orion-<version>-ubuntu-latest.zip` and its `.sha256` sidecar. Check the hash
with `shasum -a 256` on macOS or `sha256sum` on Linux, then extract the archive:

```bash
unzip orion-v0.1.0-ubuntu-latest.zip -d ~/Applications/orion
~/Applications/orion/orion-ui
```

## Build from source

```bash
cargo build --release -p orion-ui
```

The app is `target/release/orion-ui.exe` on Windows and `target/release/orion-ui`
elsewhere. Use `cargo build --release --workspace` to build the shell, the
runtime, and `orion-rt` together.

For the headless server:

```bash
cargo build --release -p orion-core --bin orion
```

See [`orion-serve-packaging.md`](orion-serve-packaging.md) for running
`orion serve`, the systemd unit, and Tailscale exposure.

## Desktop capabilities

The native GPUI shell has no system tray, no desktop notifications, no in-app
updater, and no single-instance guard. It does not update itself: install each
release with the steps above. Multiple shells can run at once; each uses its own
runtime session through `ORION_RUNTIME_SESSION`, and one daemon per session is
enforced by a lock file. See
[`adr/desktop-shell-ownership.md`](adr/desktop-shell-ownership.md) for the
decision and its consequences.

## First run

On first start Orion imports preferences from any existing predecessor client
if present (one-shot, never overwriting later changes) and writes its own state
under `%APPDATA%\Orion` (Windows) / `$XDG_DATA_HOME/orion` (Linux).

## Uninstall

Remove the application and delete `%APPDATA%\Orion` (or the XDG data dir) to
clear saved sessions, preferences and paired devices.
