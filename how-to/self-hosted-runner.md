# How-to: bupha as a GitHub self-hosted runner

The reverse of the [`gha` dialect](gha-runner.md): instead of
bupha *reading* `.github/workflows`, this mode makes bupha a
runner that **GitHub drives** — the crate speaks enough of the
actions/runner broker protocol (registration → session → message
queue → job offer → result report) that a workflow can say
`runs-on: [self-hosted, bupha]` and GitHub schedules the job onto
the box. `bupha` is always in the runner's label set.

> **Naming (T-054/T-065).** `runner.kind = "gha"` is the workflow
> *dialect* (bupha executes workflow YAML itself); `gha-runner` is
> the *broker protocol* (GitHub's service drives bupha). Two
> different products — keep them distinct.

This is a **dev-CLI surface** today: `bupha gha-runner <FILE>`
(the `bins[]` shim carries it like `watch`/`agent`), not a
driver verb — wrap it in your own `provider.services[]` entry or
run it standalone for now.

## Configure

The runner uses its **own** config file (TOML, serde — secrets
are env-var *names*, never values), not `bupha.yaml`:

```toml
repo = "owner/repo"                    # or an https URL; required once
name = "bupha-builder-1"               # runner list label (default bupha-<host>-<pid>)
labels = ["linux", "x64"]              # extra labels on top of self-hosted/OS/arch/bupha
instance = "github.com"                # or a GHES host
token_env = "BUPHA_GHA_RUNNER_TOKEN"   # registration token env NAME (settings UI / config.sh token)
pat_env = "GITHUB_PAT"                 # optional: PAT that mints a registration token instead
credentials = ".bupha-gha-runner.credentials.json"  # persisted after first registration
workdir = ".bupha-gha-runner-work"     # per-job workspaces
max_parallelism = 1                    # informational — one job at a time today
```

Registration is one-shot: the first run trades a registration
token (from `token_env`, a `pat_env`-minted one, or
`token_file`) for credentials persisted in `credentials` —
subsequent runs reuse them. Point a repo at the runner with a
workflow job:

```yaml
jobs:
  build:
    runs-on: [self-hosted, bupha]
    steps:
      - run: echo hello
```

## Protocol coverage

Implemented: registration (incl. PAT-minted tokens and
`Default` pool lookup), session key unwrap (RSA-OAEP + AES-256),
long-poll message queue + ack, broker migration, job offers via
`acquirejob`, script steps via the job's shell, `actions/checkout`
via git, `$GITHUB_ENV`/`$GITHUB_OUTPUT`/`$GITHUB_PATH` env files,
`success()`/`failure()`/`cancelled()`/`always()` conditions, job
lease renewal, cancellation, and `completejob` with the evaluated
job `outputs:` map. Log upload uses the signed-URL append-blob
path.

Explicitly out of scope (fail loudly): node/docker actions other
than `actions/checkout`, container/service jobs, `${{ }}`
expression evaluation inside step fields (the service
pre-compiles them), the realtime websocket log feed, and
artifact/cache/upload endpoints. Full boundary:
`crates/bupha-gha-runner/src/lib.rs` module docs.

## GitHub Enterprise Server

`instance = "<ghes-host>"` (or an explicit `api_base`) points the
REST calls at `{host}/api/v3`; the broker/session endpoints come
from the tenant response, so they follow the instance
automatically. The `_apis/distributedtask` `api-version` strings
in `client.rs` are the github.com values (`6.0-preview.1` pools,
`6.0-preview.2` agents, `5.1-preview` sessions/messages) — verified
against the actions/runner protocol surface but **not yet
exercised against a live GHES**, whose pipelines service may pin
different versions. First GHES run: watch for 4xx on the session
calls; the fix is a one-line constant change per endpoint.
