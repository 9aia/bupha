#!/usr/bin/env bash
# cloudflared tunnel provider — quick tunnel (no account needed).
#
#   tunnel_url → stdout: the public https URL GitHub should POST to
#   tunnel_run → exec the tunnel (foreground; put under a supervisor)
#
# Note: quick tunnels get a fresh *.trycloudflare.com URL on every start —
# re-register the webhook after restarts (deploy/register-webhook.sh), or
# move to a named tunnel + Cloudflare account for a stable URL.
set -euo pipefail

LOCAL_URL="${TUNNEL_LOCAL_URL:-http://127.0.0.1:9000}"

tunnel_run() {
  exec cloudflared tunnel --url "$LOCAL_URL" --no-autoupdate
}

# Extracts the ephemeral URL from a running `cloudflared tunnel` log.
tunnel_url() {
  grep -o 'https://[a-z0-9-]*\.trycloudflare\.com' "$TUNNEL_LOG" | head -1
}

"${1:-tunnel_run}"
