# Architecture

bupha is a port/adapter monorepo. `bupha-core` holds the domain —
the queue, `bupha.toml`, and the `Trigger`/`StatusSink` ports — and
knows nothing about GitHub, dagger or cloudflared. Ports
`bupha-runner` and `bupha-tunnel` each have impl crates; `bupha-github`
implements both trigger (HMAC verify + push parse) and status sink.
`bupha-cli` is the binary; `bupha-deploy` holds install-time helpers.
`modules/dagger/` is the shared pipeline module (Python SDK) that
`bupha init` installs into consumer repos.

The watcher pipeline:

```
POST /hook → HMAC (constant-time) → dedup X-GitHub-Delivery
  → route by repository.full_name → per-repo queue → one global
    worker → detached worktree → spawned runner child → status posts
```

Key semantics:

- **latest-wins** (default): at most 1 pending sha per repo; a newer
  push supersedes it and the skipped sha gets an honest "error:
  superseded" status. `queue = "all"` switches to bounded FIFO (32)
  for release-style repos that must run every push.
- **cancel_running**: kills the in-flight child on a new push —
  valid only with `latest` (rejected under `all`, which would drop
  a commit that was never superseded).
- **Persistence**: accepted work writes `queue.json` (tmp+rename) in
  `worktree.base`; a restart re-queues the interrupted `running` sha
  at the front.
- **Ingress is decoupled**: the listener binds localhost; a `Tunnel`
  child (`cloudflared` today) is supervised — reaped and respawned
  on exit. `direct`/`none` are for boxes already public.
- **Config names, never stores, secrets**: `secret_env`/`token_env`
  hold env-var names; values resolve env → OS keyring → env file,
  and reach the runner only via its own mechanism (e.g. `env://`
  secret for dagger).

Design rule for extending: a new forge = a `Trigger`+`StatusSink`
impl crate; a new pipeline = a `Runner` impl; a new ingress = a
`Tunnel` impl. Core stays untouched.

One **global worker** serializes runs across every repo — a second
push queues behind the in-flight job, it does not run in parallel.
That is deliberate (one box, bounded resource use), not a hang:
`journalctl --user -u bupha-watch` shows the queue moves.

## Extension seams (0.2+, by design — not yet ports)

| Coupling | Where | Planned port |
|---|---|---|
| OS service mgmt | `bupha-deploy` (systemd, df, journalctl) | `Service` — launchd/Windows later |
| Secret storage | `secret()` — env → secret-tool → file | `keyring` crate backends (macOS/Win) |
| Config path | `~/.config/bupha` (XDG) | `dirs` crate for native paths |
| VCS ops | `git()`/`repo_sync` in cli shell to git | `Vcs` trait → `GitVcs` (jj/hg later) |
| Queue persistence | `queue.json` load/save in cli | `QueueStore` — SQLite/remote for distributed runners |
