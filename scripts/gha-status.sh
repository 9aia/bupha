#!/usr/bin/env bash
# bupha GitHub Action — read or wait for a bupha commit status.
#
# bupha reports CI verdicts as forge commit statuses (default context
# "ci/bupha", states pending|success|failure|error). This script queries
# the forge's combined-status API — no bupha listener required, so it
# works from any runner that can reach the forge API.
#
# Config via environment (action.yml maps inputs to these):
#   BUPHA_MODE      status | wait          (default: status)
#   BUPHA_SHA       commit sha              (required)
#   BUPHA_CONTEXT   status context          (default: ci/bupha)
#   BUPHA_REPO      owner/repo              (required)
#   BUPHA_TOKEN     API token               (optional; unauthenticated
#                                          works for public repos)
#   BUPHA_API_BASE  forge API base          (default: https://api.github.com)
#   BUPHA_TIMEOUT   wait-mode deadline, secs (default: 600)
#   BUPHA_INTERVAL  poll interval, secs      (default: 15)
#   GITHUB_OUTPUT   set by Actions; outputs written when present
#
# Outputs (also printed to stdout as key=value):
#   state         pending|success|failure|error|none|timeout
#   description   latest status description for the context
#   target-url    status target_url (e.g. bupha run / log link)
#
# Exit status: 0 when mode=status, or wait reaches success|none→? see
# below. wait exits non-zero on failure|error|timeout.

set -euo pipefail

MODE="${BUPHA_MODE:-status}"
SHA="${BUPHA_SHA:-${GITHUB_SHA:-}}"
CONTEXT="${BUPHA_CONTEXT:-ci/bupha}"
REPO="${BUPHA_REPO:-${GITHUB_REPOSITORY:-}}"
TOKEN="${BUPHA_TOKEN:-${GH_TOKEN:-${GITHUB_TOKEN:-}}}"
API_BASE="${BUPHA_API_BASE:-https://api.github.com}"
TIMEOUT="${BUPHA_TIMEOUT:-600}"
INTERVAL="${BUPHA_INTERVAL:-15}"

case "$MODE" in
  status|wait) ;;
  *) echo "bupha-status: unknown mode '$MODE' (want status|wait)" >&2; exit 2 ;;
esac

# wait-mode knobs must be integers in range — a bad value would
# otherwise die mid-loop inside arithmetic/sleep, and interval=0
# would hot-spin the forge API until the deadline.
case "$TIMEOUT" in
  ''|*[!0-9]*) echo "bupha-status: timeout='$TIMEOUT' is not a non-negative integer" >&2; exit 2 ;;
esac
case "$INTERVAL" in
  ''|*[!0-9]*) echo "bupha-status: interval='$INTERVAL' is not a non-negative integer" >&2; exit 2 ;;
esac
if [ "$INTERVAL" -lt 1 ]; then
  echo "bupha-status: interval must be >= 1 second" >&2
  exit 2
fi
# Cap the deadline well below int64 range so SECONDS+TIMEOUT can't
# overflow into a past instant. The length guard keeps `[` from
# choking on digit strings too big for its integer type.
if [ "${#TIMEOUT}" -gt 5 ] || [ "$TIMEOUT" -gt 86400 ]; then
  echo "bupha-status: timeout must be <= 86400 seconds (24h)" >&2
  exit 2
fi

if [ -z "$SHA" ]; then
  echo "bupha-status: no sha — set BUPHA_SHA or run in Actions" >&2
  exit 2
fi
if [ -z "$REPO" ]; then
  echo "bupha-status: no repo — set BUPHA_REPO or run in Actions" >&2
  exit 2
fi

API_BASE="${API_BASE%/}"
URL="${API_BASE}/repos/${REPO}/commits/${SHA}/status"

# One combined-status fetch. Prints the raw JSON body on stdout.
fetch_status() {
  local args=(
    -fsSL
    -H "Accept: application/vnd.github+json"
    -H "X-GitHub-Api-Version: 2022-11-28"
    -H "User-Agent: bupha-gha-action"
  )
  if [ -n "$TOKEN" ]; then
    args+=(-H "Authorization: Bearer ${TOKEN}")
  fi
  curl "${args[@]}" "$URL"
}

# Reduce a combined-status response to the latest entry for $CONTEXT.
# Sets STATE / DESC / TARGET globally; STATE=none when absent.
STATE="" DESC="" TARGET=""
extract() {
  local resp="$1" entry
  # An unparseable body is a fetch failure, not "no status" —
  # callers treat extract's nonzero as transient (wait) or fatal
  # (status).
  entry=$(jq -c --arg ctx "$CONTEXT" '
    [.statuses[]? | select(.context == $ctx)]
    | sort_by(.created_at)
    | last // null
  ' 2>/dev/null <<<"$resp") || return 1
  if [ "$entry" = "null" ] || [ -z "$entry" ]; then
    STATE="none"; DESC=""; TARGET=""
    return
  fi
  STATE=$(jq -r '.state // "none"' <<<"$entry")
  # Sanitize inside jq so a trailing newline doesn't become a
  # trailing space in the output.
  DESC=$(jq -r '.description // "" | gsub("[\\n\\r\\t]"; " ")' <<<"$entry")
  TARGET=$(jq -r '.target_url // ""' <<<"$entry")
}

emit() {
  # Single-line-safe outputs for $GITHUB_OUTPUT and stdout.
  printf 'state=%s\n' "$STATE"
  printf 'description=%s\n' "$DESC"
  printf 'target-url=%s\n' "$TARGET"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
      printf 'state=%s\n' "$STATE"
      printf 'description=%s\n' "$DESC"
      printf 'target-url=%s\n' "$TARGET"
    } >>"$GITHUB_OUTPUT"
  fi
}

is_terminal() {
  case "$1" in
    success|failure|error) return 0 ;;
    *) return 1 ;;
  esac
}

case "$MODE" in
  status)
    # One-shot mode stays fail-loud, but report a clean error
    # instead of leaking jq/curl exit codes.
    if ! { RESP="$(fetch_status)" && extract "$RESP"; }; then
      echo "bupha-status: could not fetch/parse status for ${REPO}@${SHA}" >&2
      exit 1
    fi
    emit
    ;;
  wait)
    deadline=$(( SECONDS + TIMEOUT ))
    while :; do
      # A transport/API blip or an unparseable body is transient —
      # keep polling until the deadline rather than killing a long
      # wait on one failed fetch.
      if ! { RESP="$(fetch_status)" && extract "$RESP"; }; then
        if [ "$SECONDS" -ge "$deadline" ]; then
          STATE="timeout"; DESC=""; TARGET=""
          emit
          exit 1
        fi
        sleep "$INTERVAL"
        continue
      fi
      if is_terminal "$STATE"; then
        emit
        case "$STATE" in
          success) exit 0 ;;
          *) exit 1 ;; # failure | error
        esac
      fi
      if [ "$SECONDS" -ge "$deadline" ]; then
        STATE="timeout"; DESC=""; TARGET=""
        emit
        exit 1
      fi
      sleep "$INTERVAL"
    done
    ;;
esac
