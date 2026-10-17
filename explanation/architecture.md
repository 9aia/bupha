---
url: /bupha/explanation/architecture.md
---
# Architecture

bupha is a port/adapter monorepo: every external concern sits
behind a trait (a *port*), and each integration is a crate that
implements it (an *impl*). `bupha-core` is the pure domain — config
shape, queue semantics, shared primitives — and depends on nothing
outside itself; the binary is a thin composition root that wires
impls to ports.

## Crate map

| crate | layer | role |
|---|---|---|
| `bupha-core` | domain | bupha domain core: config types, job queues, shared primitives — no ports, no integrations |
| `bupha-runner` | port | Runner port: how a job becomes a spawned process |
| `bupha-status` | port | StatusSink port: where run outcomes are reported |
| `bupha-trigger` | port | Trigger port: where pushes come from |
| `bupha-tunnel` | port | Tunnel port: expose the webhook listener publicly |
| `bupha-cloudflared` | impl | Tunnel impl: cloudflared (named tunnel preferred, quick tunnel fallback) |
| `bupha-config` | impl | Config port: where bupha.toml-shaped config comes from |
| `bupha-config-toml` | impl | Config impl: bupha.toml file (explicit path, env, ./, ~/.config) |
| `bupha-dagger` | impl | Runner impl: `dagger call &lt;fn&gt;` per job |
| `bupha-dotenv` | impl | Secret impl: KEY=VAL env file (chmod 600) |
| `bupha-git` | impl | Vcs impl: git via the git CLI |
| `bupha-github` | impl | GitHub impl: push-webhook trigger + commit-status sink |
| `bupha-keyring` | impl | Secret impl: OS keyring via secret-tool (libsecret) |
| `bupha-queue` | impl | QueueStore port: queue persistence |
| `bupha-queue-redb` | impl | QueueStore impl: redb embedded store (single file, ACID) |
| `bupha-secret` | impl | Secret port: resolve named secrets from storage |
| `bupha-service` | impl | Service port: OS service lifecycle |
| `bupha-shell` | impl | Runner impl: bash -c \<cmd> per job — no containers |
| `bupha-systemd` | impl | Service impl: systemd user units |
| `bupha-vcs` | impl | Vcs port: VCS operations for repos and worktrees |
| `bupha` | binary | bupha — the oxpecker on your repo's back: headless CI CLI |

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
  → 6. route — repository.full_name → watched [[repo]] entry;
       git_ref must equal repo.branch
  → 7. enqueue — per-repo queue; atomic queue file (redb) persist
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

Persistence: accepted work writes `queue file (redb)` via tmp+rename inside
`worktree.base`; on restart the interrupted `running` sha is
re-queued at the front.

One **global worker** serializes runs across all repos — a second
push queues behind the in-flight job. Deliberate (one box, bounded
resource use), not a hang: `journalctl --user -u bupha-watch` shows
the queue moving.

## Security model

* mandatory HMAC (constant-time compare) — unauthenticated payloads
  never reach the queue
* body cap, delivery-id replay dedup, hex-sha validation before the
  value touches paths or status URLs
* webhook takeover is exact-URL only; hook API calls are timed out
* config names secrets (`secret_env`/`token_env`) — values resolve
  env → OS keyring → `~/.config/bupha/env` (0600); the runner gets
  the token through its own mechanism (`env://` for dagger)

## Extending

| to add… | write… | core changes? |
|---|---|---|
| forge (gitea/gitlab) | `Trigger` + `StatusSink` impl crate | no |
| pipeline backend | `Runner` impl crate | no |
| ingress (fly/tailscale) | `Tunnel` impl crate | no |

Planned seams (0.2+) → [project/SEAMS.md](https://github.com/9aia/bupha/blob/main/project/SEAMS.md).
