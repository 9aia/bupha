---
url: /bupha/tutorials/first-run.md
---
# Tutorial: your first bupha run

Goal: push-triggered CI on a repo, end to end, in ~10 minutes.

Prereqs: **Linux + systemd** (script runner needs bash; macOS/Windows
unsupported for now); git;
`cargo install bupha`; docker only if you use the dagger runner.
Also a stable public URL for the webhook — named cloudflared tunnel
or a direct box; quick tunnels are convenient but flaky.

1. `cd your-repo && bupha init` — scaffolds the dagger module +
   delegate (the module builds a Rust workspace — needs `cargo` in
   the pipeline image; for anything else write `ci.sh` and use the
   script runner).
2. `bupha deploy env` → set `BUPHA_WEBHOOK_SECRET` + `BUPHA_TOKEN`
   (a repo-scoped PAT) — in `~/.config/bupha/env`, or store them in
   your OS keyring with `secret-tool` (resolution: env → keyring → file).
3. Write `bupha.toml` (see `examples/bupha.toml.example`): your repo's
   `[[repos]]` entry, `runner.kind`, `tunnel`.
4. `bupha doctor` — checks this box (binaries, secrets, config) before
   you commit; fix anything red.
5. `bupha deploy service` — systemd unit written + enabled
   (`loginctl enable-linger` once for boot persistence).
6. `bupha deploy webhook` — registers `POST <public_url>/hook` on the
   repo; `bupha deploy check` verifies unit+secrets+endpoint.
7. Push. `ci/bupha` commit status appears; superseded/cancelled shas
   get honest error statuses.

Done. Logs: `journalctl --user -u bupha-watch -f`.
