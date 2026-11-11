# Tutorial: your first bupha run

Goal: push-triggered CI on a repo, end to end, in ~10 minutes.

bupha ships as a tamias `type: bupha` bundle — there is no
user-facing `bupha` CLI (ADR-0006 amendment). You install the
bundle with tamias, the provider unit runs the watcher engine
through `tamias-driver-bupha host`, and GitHub pushes flow
webhook → queue → worktree → pipeline → commit status.

Prereqs: **Linux + systemd** (provider units need systemd; the
shell runner needs bash; macOS/Windows unsupported for now); git;
the `tamias` CLI and the `tamias-driver-bupha` driver binary on
`PATH` or in `$TAMIAS_HOME/drivers/` (`cargo build --release -p
bupha-driver` from this repo produces it); a stable public URL
for the webhook — named cloudflared tunnel or a direct box; quick
tunnels are convenient but flaky. Docker only if you use the
dagger runner.

The shipped bundles live in `libraries/` — `ci-gate` (push gate
on `main`) and `docs-deploy` (push to `docs` → site deploy). This
tutorial walks `ci-gate`; swap the name for your own bundle.

1. **Secrets first** — names only in config, values in cuora
   (canonical, T-056):

   ```sh
   printf '%s' "$BUPHA_TOKEN_VALUE"   | cuora put cuora://bupha/BUPHA_TOKEN
   printf '%s' "$WEBHOOK_SECRET_VALUE" | cuora put cuora://bupha/BUPHA_WEBHOOK_SECRET
   ```

   (`BUPHA_TOKEN` is a repo-scoped PAT for statuses + hook
   management; `BUPHA_WEBHOOK_SECRET` is the HMAC secret GitHub
   signs deliveries with. Fallbacks when cuora isn't available:
   OS keyring, then `~/.config/bupha/env` — legacy.)

2. `tamias install <ref>` — fetch + unpack the bundle payload
   (driver `install` is a no-op; the payload's `fleet.toml` +
   `tamias.yaml` are the config sources).

3. `tamias deploy ci-gate` — renders `bupha-watch.service` **and**
   runs the driver's `deploy` verb: merges `fleet.toml` with the
   manifest's `provider.ingress` and writes
   `~/.config/bupha/bundles/ci-gate.toml` — the file the unit's
   `tamias-driver-bupha host --config …` consumes.

4. `tamias driver exec ci-gate hooks register --dry-run` — print
   the webhook plan, then `… hooks register` for real (needs the
   secrets above). This wires GitHub → the bundle's public URL —
   **standalone/dev ingress** (ADR-0007); production ingress is a
   peregrin campaign listener.

5. `tamias service enable bupha-watch` — start the watcher
   (`loginctl enable-linger` once for boot persistence).

6. `tamias driver exec ci-gate health` — ping `/hook` + unit
   probe (JSON); `tamias driver exec ci-gate status` — per-unit
   is-active/is-enabled; `tamias driver exec ci-gate logs` —
   journalctl over the provider units.

7. Push. `ci/bupha` commit status appears; superseded/cancelled
   shas get honest error statuses.

Done. Logs: `journalctl --user -u bupha-watch -f` (or
`tamias driver exec ci-gate logs --lines 200`).

## Local reproduction (dev harness)

To reproduce a red run without touching the deployment, use the
dev CLI — `cargo install --git https://github.com/9aia/bupha
bupha`, then in the repo: `bupha doctor` checks the box against a
config, `bupha run` executes the configured runner in the watched
`dir` — the same pipeline, locally. `bupha watch` runs the whole
engine standalone (no tamias, no peregrin) — the ADR-0007
fallback ingress, not the product path.
