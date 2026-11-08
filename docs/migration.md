# Migration notes — upgrading Orion

## Upgrading between releases

Orion updates are shipped by pulling this repository. There is no release channel or automatic updater triggered by the app itself; you build from source or run the installers from a GitHub release.

### What migrates automatically

- **Preferences:** on first start after an upgrade, Orion imports an existing predecessor client preferences document (one-shot, never overwriting later changes) if one is present.
- **State:** `%APPDATA%\Orion` (Windows) or `$XDG_DATA_HOME/orion` (Linux) is preserved across upgrades. Saved sessions, preferences, and device pairings live there.
- **Agent CLIs:** Orion discovers agents on `PATH`. Missing CLIs are simply not offered; no reinstallation is required.

### What you must do manually

- Reinstall the desktop app (or rebuild from source) for each release; there is no in-app updater yet.
- For headless hosts, copy the new `orion` binary over the previous one and restart the service (`systemctl restart orion` or equivalent).

### Breaking changes

None in v0.1.0. Protocol revisions, when they happen, are documented in `src/types/protocol.ts` as a wire contract change, not a convenience edit.
