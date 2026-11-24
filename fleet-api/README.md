# fleet-api

Authenticated tailnet-only HTTP JSON backend and Ad Hoc OTA IPA server for the native Fleet iOS app.

## Security Model

- Binds strictly to `127.0.0.1:8694` and is published only inside the tailnet via `tailscale serve` (never `tailscale funnel`).
- Read endpoints read local state JSON files directly in memory and never spawn subprocesses.
- Write actions (`/v1/actions/*`) use a strict allow-list with regex/length checks and append every action to `~/.local/state/fleet-api/actions.jsonl`.
- Authentication checks a 256-bit Bearer token (`Authorization: Bearer ...` or `X-Fleet-Token`) using `hmac.compare_digest`. `/v1/ota/*` accepts `?t=<token>` for iOS Safari / `itms-services://` and returns silent `404` on invalid tokens.

For the authenticated service directory and the equivalent local CLI, see [Find a running service](../docs/stack.md#find-a-running-service).
