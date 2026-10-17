# Architecture

bupha is a port/adapter monorepo: `bupha-core` owns the domain and
knows nothing about GitHub, dagger, or cloudflared; every integration
is an impl crate behind a port.

## Crate map

| crate | layer | role |
|---|---|---|
| `bupha-core` | domain | `bupha.toml` types, `Queue`, replay `Seen`, sha validation — no ports, no impls |
| `bupha-runner` | port | `Runner` trait — spawn/kill a pipeline child |
| `bupha-tunnel` | port | `Tunnel` trait — expose the listener |
| `bupha-trigger` | port | `Trigger` trait + `PushOutcome` |
| `bupha-status` | port | `StatusSink` trait — run outcomes |
| `bupha-github` | impl | webhook verify + parse (Trigger), commit statuses (StatusSink), hook register/delete |
| `bupha-script` | impl | `bash -c <cmd>` in the worktree — zero containers |
| `bupha-dagger` | impl | `dagger call` into the consumer repo's module |
| `bupha-cloudflared` | impl | cloudflared child (quick + named tunnels) |
| `bupha-deploy` | impl | systemd/env/publish helpers — impl of the planned `Service` port (not yet a trait) |
| `bupha` (cli) | binary | wires it all: `watch`, `run`, `doctor`, `init`, `deploy`, `destroy` |

`modules/dagger/` (Python SDK) is the shared pipeline module
`bupha init` installs into consumer repos — pinned to a sha.

## Request flow

```
POST /hook
  → 1. path check (404 otherwise)
  → 2. read body (≤ 2 MiB)
  → 3. HMAC verify — sha256, constant-time
  → 4. dedup X-GitHub-Delivery (bounded 10k ring)
  → 5. parse — skip deletes/zero-sha; reject non-hex sha
  → 6. route — repository.full_name → watched [[repos]] entry;
       git_ref must equal repo.branch
  → 7. enqueue — per-repo queue; atomic queue.json persist
  → 8. one global worker — git fetch, detached worktree
       at <base>-<repo>-<sha12>
  → 9. spawn runner child (token via env:// name, never argv)
  → 10. status — pending → success/error; superseded and
        cancelled shas get honest error statuses
```

## Queue semantics

| policy | pending | on new push | use for |
|---|---|---|---|
| `latest` (default) | 1 sha | supersedes pending; `cancel_running` may kill in-flight | CI gates |
| `all` | bounded FIFO (32) | appends; oldest evicted past cap; `cancel_running` rejected at load — it would drop a promised commit | releases |

Persistence: accepted work writes `queue.json` via tmp+rename inside
`worktree.base`; on restart the interrupted `running` sha is
re-queued at the front.

One **global worker** serializes runs across all repos — a second
push queues behind the in-flight job. Deliberate (one box, bounded
resource use), not a hang: `journalctl --user -u bupha-watch` shows
the queue moving.

## Security model

- mandatory HMAC (constant-time compare) — unauthenticated payloads
  never reach the queue
- body cap, delivery-id replay dedup, hex-sha validation before the
  value touches paths or status URLs
- webhook takeover is exact-URL only; hook API calls are timed out
- config names secrets (`secret_env`/`token_env`) — values resolve
  env → OS keyring → `~/.config/bupha/env` (0600); the runner gets
  the token through its own mechanism (`env://` for dagger)

## Extending

| to add… | write… | core changes? |
|---|---|---|
| forge (gitea/gitlab) | `Trigger` + `StatusSink` impl crate | no |
| pipeline backend | `Runner` impl crate | no |
| ingress (fly/tailscale) | `Tunnel` impl crate | no |

## Planned seams (0.2+)

| coupling | where | planned port |
|---|---|---|
| OS service mgmt | `bupha-deploy` (systemd, df, journalctl) | `Service` — launchd/Windows |
| secret storage | `secret()` — secret-tool | `keyring` crate (macOS/Win) |
| config path | `~/.config/bupha` (XDG) | `dirs` crate |
| VCS ops | `git()`/`repo_sync` shell to git | `Vcs` trait → `GitVcs` (jj/hg) |
| queue persistence | `queue.json` in cli | `QueueStore` — SQLite/remote for distributed runners |
