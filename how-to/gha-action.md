# How-to: use bupha as a GitHub Action

`uses: 9aia/bupha` is a composite action at the bupha repo root —
bash + curl/jq only, no build step and **no bupha listener needed**.
It reads the forge's combined commit-status API, which is where
bupha's watcher posts its verdicts (`[status].context`, default
`ci/bupha`).

Typical use: a repo's own GHA workflow waits for — or reports — the
bupha gate verdict on a sha, without checking out anything or
reaching the bupha box.

## Gate on the bupha verdict

```yaml
permissions:
  contents: read
  statuses: read          # required for private repos

jobs:
  bupha-gate:
    runs-on: ubuntu-latest
    steps:
      - uses: 9aia/bupha@main    # pin to a tag or SHA in production
        with:
          mode: wait
          timeout: "900"
```

`mode: wait` polls
`GET /repos/{owner}/{repo}/commits/{sha}/status`, picks the latest
entry matching `context` (default `ci/bupha`), and exits non-zero
when the status reaches `failure`, `error`, or the timeout elapses —
the step is a drop-in required check.

## Report the verdict without failing

```yaml
- uses: 9aia/bupha@main
  id: bupha
  with:
    mode: status            # one fetch, always exit 0
- run: echo "bupha: ${{ steps.bupha.outputs.state }} — ${{ steps.bupha.outputs.description }}"
```

Outputs:

| output | values |
| ------ | ------ |
| `state` | `pending` `success` `failure` `error` `none` `timeout` |
| `description` | the status description bupha posted |
| `target-url` | status `target_url` (run/log link) |

`none` means nothing was posted for that sha/context yet — usually
the webhook hasn't landed or the repo isn't watched. In `wait` mode
`none` is treated as not-terminal and keeps polling.

## Inputs

| input | default | notes |
| ----- | ------- | ----- |
| `mode` | `wait` | `status` (one shot) or `wait` (poll to terminal) |
| `sha` | `${{ github.sha }}` | for `pull_request`, prefer `github.event.pull_request.head.sha` |
| `context` | `ci/bupha` | must match the bundle's `[status].context` |
| `repo` | `${{ github.repository }}` | `owner/repo` |
| `token` | `${{ github.token }}` | needs `statuses: read` on private repos |
| `api-base` | `https://api.github.com` | GHES override |
| `timeout` | `600` | seconds, wait mode |
| `interval` | `15` | seconds between polls |

## What it does *not* do

There is no `trigger` mode. Firing a bupha run from GHA would mean a
public, unauthenticated ingress — production webhook/forge ingress is
peregrin-owned and the bupha webhook listener requires an HMAC
signature (ADR-0007). To kick CI off, push to a watched ref, use the
bundle's `hooks register`, or drive a peregrin campaign.

## Manual test

No live GitHub needed — stub `curl` and exercise the script:

```sh
mkdir /tmp/fake && cat > /tmp/fake/curl <<'EOF'
#!/usr/bin/env bash
cat <<'JSON'
{"state":"success","statuses":[
 {"context":"ci/bupha","state":"success","description":"ci passed",
  "target_url":"https://example/run/1","created_at":"2025-01-01T00:00:00Z"}]}
JSON
EOF
chmod +x /tmp/fake/curl
PATH=/tmp/fake:$PATH BUPHA_MODE=status BUPHA_SHA=abc BUPHA_REPO=o/r \
  GITHUB_OUTPUT=/tmp/out.txt \
  bash scripts/gha-status.sh && cat /tmp/out.txt
```
