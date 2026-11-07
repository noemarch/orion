# Packaging and running `orion serve`

Orion's headless surface is the `orion` binary built from `orion-core`. It serves the exact same
React 19 UI the Tauri desktop shell uses, over plain HTTP/WebSockets, so a phone, tablet, or any
browser on the tailnet can open workspaces, read panes, and drive agents without a local desktop.

## Build the production binary

Build a release binary for the host machine:

```bash
cargo build --release -p orion-core --bin orion
```

The artifact lands at `target/release/orion.exe` (Windows) or `target/release/orion` (Unix). Copy it
to the headless host like any other system binary; there are no runtime dependencies beyond the
agent CLIs Orion drives (`claude`, `codex`, …), which must already be authenticated on `PATH`.

## Run headless

```bash
ORION_TOKEN=... orion serve --host 0.0.0.0 --port 7317 --state-dir /var/lib/orion --dist dist
```

| Flag | Purpose |
|------|---------|
| `ORION_TOKEN=...` | Device-pairing token the browser gateway checks. Set it to a strong secret; it is never echoed in responses or logs. |
| `--host 0.0.0.0` | Listen on all interfaces so LAN/Tailscale clients can reach it. |
| `--port 7317` | The port Orion's HTTP/WebSocket gateway binds. |
| `--state-dir /var/lib/orion` | Where saved sessions, preferences, and device pairings live. Make it writable by the service user. |
| `--dist dist` | Directory holding the built React UI (`dist/` from the frontend build) served to browsers. |

## systemd unit (headless host)

Create `/etc/systemd/system/orion.service`:

```ini
[Unit]
Description=Orion headless agent orchestrator
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=orion
Group=orion
WorkingDirectory=/var/lib/orion
EnvironmentFile=-/etc/orion/orion.env
ExecStart=/usr/local/bin/orion serve --host 0.0.0.0 --port 7317 --state-dir /var/lib/orion --dist /usr/local/share/orion/dist
Restart=on-failure
RestartSec=5

# No telemetry, no phone home: Orion never prompts users or reports usage.
Environment=ORION_NO_TELEMETRY=1

[Install]
WantedBy=multi-user.target
```

Put `ORION_TOKEN=...` in `/etc/orion/orion.env` (mode `0600`, owned by the `orion` user), then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now orion
```

## Windows service note

- **Desktop shell:** use Tauri's `tauri-plugin-autostart` so the Orion tray app starts with the user session. That is the graphical surface, not the headless gateway.
- **Headless on Windows:** there is no built-in Windows service wrapper. Run `orion serve` as a scheduled task set to "Run whether user is logged on or not", or install it as a service with NSSM pointing at `orion.exe serve --host 0.0.0.0 --port 7317 --state-dir <...> --dist <...>`, with `ORION_TOKEN` set in the task/service environment.

## Tailscale (remote and phone access)

On the headless host, expose the local gateway over the tailnet with HTTPS:

```bash
tailscale serve --bg --https=443 http://127.0.0.1:7317
```

Clients reach Orion at `https://<host>.ts.net/` (or the registered MagicDNS name). Browsers and the
mobile PWA connect over the Tailscale mesh; no ports are opened to the public internet.

**Never run `tailscale funnel`** for Orion. `funnel` exposes a local port to the public internet,
which would put an authenticated agent-control surface on the open web. `tailscale serve --https`
keeps the service tailnet-only, which is the only supported remote-exposure mode.
