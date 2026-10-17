#!/usr/bin/env bash
# direct tunnel provider — box already has a public IP / port forward.
# TUNNEL_PUBLIC_URL is the external address GitHub should POST to.
set -euo pipefail

tunnel_run() {
  echo "direct: nothing to run — endpoint is $TUNNEL_PUBLIC_URL" >&2
  sleep infinity
}

tunnel_url() { echo "$TUNNEL_PUBLIC_URL"; }

"${1:-tunnel_run}"
