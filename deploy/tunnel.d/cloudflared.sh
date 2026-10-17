#!/usr/bin/env bash
# cloudflared tunnel provider.
#
#   tunnel_run → exec the tunnel (foreground; put under a supervisor)
#   tunnel_url → stdout: the public https URL GitHub should POST to
#
# Modes:
#   named (stable URL, preferred):  TUNNEL_NAME=handl-ci
#     Requires a Cloudflare account: `cloudflared tunnel login`, then
#     `cloudflared tunnel create handl-ci` +
#     `cloudflared tunnel route dns handl-ci ci.<domain>` once, and a
#     ~/.cloudflared/config.yml ingress mapping the hostname to the
#     local listener. URL survives restarts — webhook registered once.
#   quick (throwaway URL):          TUNNEL_NAME unset
#     Fresh *.trycloudflare.com URL per start — re-register the webhook
#     after every restart (deploy/register-webhook.sh).
set -euo pipefail

LOCAL_URL="${TUNNEL_LOCAL_URL:-http://127.0.0.1:9000}"

tunnel_run() {
  if [ -n "${TUNNEL_NAME:-}" ]; then
    exec cloudflared tunnel run "$TUNNEL_NAME"
  fi
  exec cloudflared tunnel --url "$LOCAL_URL" --no-autoupdate
}

tunnel_url() {
  if [ -n "${TUNNEL_PUBLIC_URL:-}" ]; then
    echo "$TUNNEL_PUBLIC_URL"          # stable named-tunnel URL
  else
    grep -o 'https://[a-z0-9-]*\.trycloudflare\.com' "$TUNNEL_LOG" | head -1
  fi
}

"${1:-tunnel_run}"
