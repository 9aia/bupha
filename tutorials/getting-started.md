---
url: /bupha/tutorials/getting-started.md
---
# Getting started

Push-triggered CI on a repo, end to end, in ~10 minutes.

bupha ships as tamias packages — the fleet is a `type: bupha`
bundle and the `bupha` command is a `bins[]` shim pack
(`libraries/bupha`, ADR-0006 amendment + T-012). You install the
bundle with tamias, the provider unit runs the watcher engine
through `tamias-driver-bupha host`, and GitHub pushes flow
webhook → queue → worktree → pipeline → commit status.

Prereqs: **Linux + systemd** (provider units need systemd; the
shell runner needs bash; macOS/Windows unsupported for now); git;
the `tamias` CLI; the `bupha` command pack — `tamias install
9aia:bupha@<version>` from the org registry (needs
`TAMIAS_REGISTRY_TOKEN`), or `scripts/pack.sh` + `tamias install --allow-drivers ./libraries/bupha` from a checkout — which
registers `tamias-driver-bupha` into `$TAMIAS_HOME/drivers/`
(`cargo build --release -p bupha-driver` from this repo produces
the binary by hand); a stable public URL
for the webhook — named cloudflared tunnel or a direct box; quick
tunnels are convenient but flaky. Docker only if you use the
dagger runner.

The shipped bundles live in `libraries/` — `ci-gate` (push gate
on `main`) and `docs-deploy` (push to `docs` → site deploy). This
tutorial walks `ci-gate`; swap the name for your own bundle.

## 1. Secrets first — names only in config, values in cuora

Canonical store is cuora (T-056):

```sh
printf '%s' "$BUPHA_TOKEN_VALUE"   | cuora put cuora://bupha/BUPHA_TOKEN
printf '%s' "$WEBHOOK_SECRET_VALUE" | cuora put cuora://bupha/BUPHA_WEBHOOK_SECRET
```

(`BUPHA_TOKEN` is a repo-scoped PAT for statuses + hook
management; `BUPHA_WEBHOOK_SECRET` is the HMAC secret GitHub
signs deliveries with. Fallback when cuora isn't available:
`~/.config/bupha/env` — the dev-mode / no-cuora leg (the
OS-keyring leg was dropped post-T-077).)

## 2. Install the bundle

`tamias install <ref>` — fetch + unpack the bundle payload
(driver `install` is a no-op; the payload's `fleet.toml` +
`tamias.yaml` are the config sources).

## 3. Deploy it

`tamias deploy ci-gate` — renders `bupha-watch.service` **and**
`bupha-worker.service` (one ingress mode each — enable ONE,
see [tamias bundles](/how-to/tamias-bundles)) **and**
runs the driver's `deploy` verb: merges `fleet.toml` with the
manifest's `provider.ingress` and writes
`~/.config/bupha/bundles/ci-gate.toml` — the file both units'
`tamias-driver-bupha host|worker --config …` consume.

## 4. Register the webhook

`tamias driver exec ci-gate hooks register --dry-run` — print
the webhook plan, then `… hooks register` for real (needs the
secrets above). This wires GitHub → the bundle's public URL —
**standalone/dev ingress** (ADR-0007); production ingress is a
peregrin campaign listener.

## 5. Start the watcher

`tamias service enable bupha-watch` — start the standalone
watcher (`loginctl enable-linger` once for boot persistence);
`bupha-worker` is the substrate/production alternative — see
[distributed runners](/how-to/distributed-runners).

## 6. Check health

```sh
tamias driver exec ci-gate health   # ping /hook + unit probe (JSON)
tamias driver exec ci-gate status   # per-unit is-active/is-enabled
tamias driver exec ci-gate logs     # journalctl over the provider units
```

## 7. Push

`ci/bupha` commit status appears; superseded/cancelled
shas get honest error statuses.

Done. Logs: `journalctl --user -u bupha-watch -f` (or
`tamias driver exec ci-gate logs --lines 200`).

## What you now know

* bupha is tamias-distributed: bundles are `type: bupha` content
  packs; the product surface is `tamias install`/`deploy` plus
  `tamias driver exec <bundle> health|status|logs|hooks`, with the
  watcher engine running inside the provider unit via
  `tamias-driver-bupha host`.
* Secrets are names in config, values in cuora (`cuora://` refs)
  with `~/.config/bupha/env` as the dev-mode fallback.
* One bundle deploys two units (`bupha-watch`, `bupha-worker`) —
  you enable the ingress mode you want; GitHub webhooks flow
  HMAC-verified into a dedup'd queue → detached worktree →
  pipeline → commit status.

## Local reproduction (dev harness)

To reproduce a red run without touching the deployment, use the
dev CLI — install the command pack (`scripts/pack.sh` then
`tamias install --allow-drivers ./libraries/bupha` from this
repo), then in the repo: `bupha doctor` checks the box against a
config, `bupha run` executes the configured runner in the watched
`dir` — the same pipeline, locally. `bupha watch` runs the whole
engine standalone (no tamias, no peregrin) — the ADR-0007
fallback ingress, not the product path.

## Where next

* **How-to** (tasks): [tamias bundles](/how-to/tamias-bundles) ·
  [multi-repo](/how-to/multi-repo) ·
  [shell runner](/how-to/shell-runner) ·
  [gha runner — your own GHA workflows](/how-to/gha-runner) ·
  [self-hosted GHA runner](/how-to/self-hosted-runner) ·
  [distributed runners](/how-to/distributed-runners) ·
  [sccache backend](/how-to/sccache-backend)
* **Reference** (lookup): [cli — dev harness](/reference/cli) ·
  [config](/reference/config) · [crate api](/reference/crate-api)
* **Explanation** (why): [architecture](/explanation/architecture) ·
  [release model](/explanation/release-model) ·
  [comparison](/comparison) — vs GHA/hosted CI ·
  [crate scope audit](/explanation/crate-scope-audit)

Project: [CONTRIBUTING](https://github.com/9aia/bupha/blob/main/CONTRIBUTING.md) ·
[SECURITY](https://github.com/9aia/bupha/blob/main/SECURITY.md) ·
[project](https://github.com/9aia/bupha/blob/main/project/index.md) — TODO, backlog, ADRs
