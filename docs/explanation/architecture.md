# Architecture

bupha is a port/adapter monorepo: `bupha-core` holds the domain
(latest-wins queue, bupha.toml, Trigger/StatusSink traits) and knows
nothing about github, dagger or cloudflared. Ports `bupha-runner` and
`bupha-tunnel` have impl crates (`bupha-dagger`, `bupha-script`,
`bupha-cloudflared`). `bupha-github` implements both trigger (HMAC
verify + push parse) and status sink (commit-status API).

The watcher: tiny_http endpoint → HMAC → route by
repository.full_name → per-repo pending slot → one global worker →
detached worktree → spawned runner child (killable via cancel_running)
→ status posts. Accepted work persists to queue.json; restarts resume
it. Tunnels are supervised children — reaped + respawned on exit.

Why latest-wins: for push-heavy mains, running every commit serially
is the wrong default — `queue = "all"` opts a repo into run-every-push
FIFO (bounded at 32; overflow evicts oldest). Per-repo policy in
`[[repos]]`; the default ""latest" collapses pending to newest. Or: for push-heavy mains — N pushes during a run should mean one follow-up
on the newest sha, not N stale builds.
