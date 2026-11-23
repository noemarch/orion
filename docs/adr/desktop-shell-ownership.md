# Desktop Shell Ownership

Decision: The native `orion-ui` binary is the desktop application. The Tauri shell is retired; `orion-ui` is the only desktop shell, and no WebView is reintroduced. The pure-Rust/no-WebView production constraint remains: the native shell is `crates/orion-ui`, driven by `orion-core::Engine`.

Consequences:

- The release pipeline ships the GPUI binary as the desktop app.
- Release packaging builds `orion-ui` as the desktop binary; no Tauri artifacts are produced.
- Tauri does not render the Orion UI and must not reintroduce WebView for UI rendering.
- The native GPUI shell does not implement a tray icon, OS/desktop notifications, an in-app updater, or a single-app-instance/focus guard. Reintroducing any of them would need native platform crates or webview-era launcher code and is out of scope for the pure-Rust shell.
- There is no UI-level single-instance guard; multiple shells are supported via `ORION_RUNTIME_SESSION`. One daemon per session is enforced by a PID lock in `crates/orion-core/src/runtime/lock.rs`.

(End of file - total 13 lines)
