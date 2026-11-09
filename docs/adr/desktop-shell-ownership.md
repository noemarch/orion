# Desktop Shell Ownership

Decision: The native `orion-ui` binary is the desktop application. Tauri is reduced to a launcher/tray host only; it is not the UI shell.

Consequences:

- The release pipeline ships the GPUI binary as the desktop app.
- Tauri does not render the Orion UI and must not reintroduce WebView for UI rendering.
- Any retained Tauri code is limited to launcher/tray responsibilities. If the release pipeline no longer needs those, Tauri can be retired entirely.
- The pure-Rust/no-WebView production constraint remains: the native shell is `crates/orion-ui`, driven by `orion-core::Engine`.
