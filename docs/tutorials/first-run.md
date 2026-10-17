# Tutorial: your first bupha run

Goal: push-triggered CI on a repo, end to end, in ~10 minutes.

Prereqs: linux box with git; `cargo install bupha`; docker only if you
use the dagger runner.

1. `cd your-repo && bupha init` — scaffolds the dagger module +
   delegate (or just write `ci.sh` for the script runner).
2. `bupha deploy env` → fill `~/.config/bupha/env`:
   `BUPHA_WEBHOOK_SECRET`, `BUPHA_TOKEN` (a repo-scoped PAT).
3. Write `bupha.toml` (see `examples/bupha.toml.example`): your repo's
   `[[repos]]` entry, `runner.kind`, `tunnel`.
4. `bupha deploy service` — systemd unit written + enabled
   (`loginctl enable-linger` once for boot persistence).
5. `bupha deploy webhook` — registers `POST <public_url>/hook` on the
   repo; `bupha deploy check` verifies unit+secrets+endpoint.
6. Push. `ci/bupha` commit status appears; superseded/cancelled shas
   get honest error statuses.

Done. Logs: `journalctl --user -u bupha-watch -f`.
