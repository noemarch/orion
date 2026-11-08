# Pairing a phone or remote browser

Orion's gateway is tailnet- or LAN-scoped. Pairing a device is a short-code
flow; no account or cloud service is involved.

## Desktop

1. Open Orion → Settings → Devices → "Pair new device".
2. A 6-digit code is shown, valid for a few minutes.
3. On the phone, open `https://<host>.ts.net/` (or the LAN URL) and enter the
   code. The device token is stored on both sides.

## Headless (`orion serve`)

Set `ORION_TOKEN` to a strong secret in the service environment, then browse to
the gateway URL and enter it once. The token is never echoed in responses or
logs.

## Security notes

- Pairing codes and device tokens are compared in constant time.
- Only `tailscale serve --https` is supported for remote exposure.
- **Never run `tailscale funnel`** for Orion: it would put an authenticated
  agent-control surface on the public internet.
- User API keys (Anthropic, OpenAI, GitHub) are stored in settings and are
  never sent in WebSocket messages or logged.
