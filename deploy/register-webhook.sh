#!/usr/bin/env bash
# register-webhook.sh — point handl-lang/platform's push webhook at a URL.
#
#   deploy/register-webhook.sh https://<tunnel-url> "$HL_WEBHOOK_SECRET"
#
# Reuses HANDL_TOKEN or gh auth. Idempotent: updates an existing hook that
# already targets the same path, otherwise creates a new one.
set -euo pipefail

URL="${1:?usage: register-webhook.sh <public-url> <secret>}"
SECRET="${2:?usage: register-webhook.sh <public-url> <secret>}"
REPO="${CI_REPO:-handl-lang/platform}"
HOOK_URL="$URL/hook"

existing=$(gh api "repos/$REPO/hooks" \
  --jq '.[] | select(.config.url | endswith("/hook")) | .id' 2>/dev/null | head -1 || true)

if [ -n "$existing" ]; then
  gh api "repos/$REPO/hooks/$existing" -X PATCH \
    -f 'config[url]='"$HOOK_URL" -f 'config[secret]='"$SECRET" \
    -f 'config[content_type]=json' -f 'events[]=push' -F active=true \
    --jq '.id' > /dev/null
  echo "updated hook $existing -> $HOOK_URL"
else
  gh api "repos/$REPO/hooks" -X POST \
    -f name=web -F active=true -f 'events[]=push' \
    -f 'config[url]='"$HOOK_URL" -f 'config[secret]='"$SECRET" \
    -f 'config[content_type]=json' --jq '.id' > /dev/null
  echo "created hook -> $HOOK_URL"
fi
