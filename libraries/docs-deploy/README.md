# docs-deploy — bupha docs-site deploys as a tamias bundle

Docs-branch deployment packaged as a tamias content bundle
(`type: bupha`). Two provider units cover the two ingress modes
(ADR-0007): `bupha-watch-docs.service` — a second watcher that
fans GitHub push webhooks into bupha's queue → detached worktree
of the pushed `docs` sha → the repo's own deploy script →
`docs/deploy` commit status, latest-wins per repo — and
`bupha-worker-docs.service` — the substrate/production consumer
(P-002) polling `kind="ci-job"` entities on the peregrin node and
running only this fleet's job keys. Enable ONE per deployment.

This is the bupha-native successor to the peregrin
`docs-deploy` campaign (and before that the GHA `deploy.yml`
workflows): same trigger shape (push to `refs/heads/docs`), same
status context (`docs/deploy`), same gh-pages delivery — the
campaign YAML's per-repo node triplets collapse into `[[repo]]`
entries because bupha's runner *is* the shell command.

## Layout

| file | role |
| --- | --- |
| `tamias.yaml` | package manifest — `provider:` declares TWO systemd units: `watch-docs` → `bupha-watch-docs.service` (`tamias-driver-bupha host --config ~/.config/bupha/bundles/docs-deploy.toml`, standalone mode) and `worker-docs` → `bupha-worker-docs.service` (the `worker` verb, same config, substrate mode), plus the env file and the public ingress (`https://docs-deploy.luisfuturist.com` → `127.0.0.1:29174/hook`) with the GitHub hooks to manage |
| `fleet.toml` | the operator config payload — the `[[repo]]` set, the `[runner]`/`[status]`/`[queue]`/`[worktree]` blocks, and `[worker] node`/`node_token_env` pointing the entity-plane worker at the peregrin node. Deploy merges the manifest-owned `tunnel.public_url`/`trigger.listen`/`trigger.kind` into it and writes `~/.config/bupha/bundles/docs-deploy.toml` |

The fleet file ships one concrete repo (`9aia/bupha`) plus a
commented template — the full org-repo import is a separate
workstream.

## Flow

```sh
tamias install <ref>          # fetch + unpack the payload; driver install is a no-op
tamias deploy docs-deploy     # render bupha-watch-docs.service +
                              # bupha-worker-docs.service AND run the driver's deploy:
                              #   fleet.toml + provider.ingress → ~/.config/bupha/bundles/docs-deploy.toml
tamias driver exec docs-deploy hooks register --dry-run   # print the webhook plan
tamias driver exec docs-deploy hooks register             # upsert GitHub hooks (needs secrets)
tamias service enable bupha-worker-docs                   # substrate mode: claim docs ci-job entities
tamias service enable bupha-watch-docs                    # standalone/dev: run the webhook watcher
tamias driver exec docs-deploy health                      # ping /hook + unit probe, JSON
tamias driver exec docs-deploy status                      # per-unit is-active/is-enabled, JSON
tamias driver exec docs-deploy logs --lines 200            # journalctl over the provider units
tamias uninstall docs-deploy      # remove the unit; driver uninstall removes bundles/docs-deploy.toml
```

`tamias driver config-schema docs-deploy` prints the JSON Schema
(draft 2020-12) of the operator config `deploy` writes.

## Two watchers, one repo

`ci-gate` and `docs-deploy` are separate bundles with separate
listeners (29173 vs 29174) and separate bundle configs — in
**standalone mode**. GitHub delivers every `push` to *both* hook
URLs; each watcher's `[[repo]].branch` filter keeps only its own
refs — `main` for the gate, `docs` for deploys. A push to `main`
costs docs-deploy one verified-then-filtered delivery; nothing
runs twice.

In **substrate mode** there are no listeners at all: the peregrin
ci-ingress campaign normalizes pushes into `ci-job` entities on the
node, and each bundle's worker unit (`bupha-worker.service`,
`bupha-worker-docs.service`) claims only the job keys its own
fleet.toml covers — same non-overlap, one hop earlier in the
pipeline.

## Repo requirements

Each `[[repo]]` needs, at its `refs/heads/docs` tip:

- a deploy entrypoint the runner `cmd` can call — convention is
  `scripts/deploy-docs.sh` (bupha's own docs branch carries one);
- for gen-dependent repos (docs:deploy runs `cargo doc`/gen
  steps), the script must target the *main* checkout — bupha only
  materializes the docs-branch worktree. bupha's `deploy-docs.sh`
  honors `BUPHA_REPO=<main clone path>` for exactly this;
- a `BUPHA_WH_<OWNER>_<REPO>` secret value (webhook HMAC) and
  `BUPHA_TOKEN` (status posts + gh-pages push auth) in the cuora
  vault as `cuora://bupha/<NAME>` — names only in config, values
  sealed in cuora (T-059; the `~/.config/bupha/env` file is
  retired).

The gh-pages push inside the deploy script authenticates via the
worktree's `origin` + an env-scoped `credential.helper` reading
`$BUPHA_TOKEN` — the token never lands in git config (see
`docs/scripts/deploy-docs.sh` on any docs branch for the pattern).

## Secrets

Names only, never values. Resolution order env → **cuora**
(`cuora://bupha/<NAME>` via the `cuora` CLI — canonical, T-056) →
`~/.config/bupha/env` (legacy code fallback; the file
is retired — T-059 — and no unit sets `EnvironmentFile`):

- `BUPHA_TOKEN` — forge token for statuses + gh-pages pushes
  (`[runner] token_env`)
- `BUPHA_WH_<OWNER>_<REPO>` — per-repo webhook secret
  (`[[repo]] secret_env`); the global fallback is
  `BUPHA_WEBHOOK_SECRET` (`[trigger] secret_env`)
- `PEREGRIN_TOKEN` — the peregrin node's bearer credential
  (`[worker] node_token_env`); only the worker unit needs it, and
  only in substrate mode

Seed/rotate values with `printf '%s' "$VALUE" | cuora put
cuora://bupha/<NAME>` — every fleet secret already lives there
(T-059).

## Driver

`tamias-driver-bupha` (this repo's `crates/bupha-driver`) is a
dual-surface binary: the pavo verbs (`manifest`/`capabilities`/
`serve`) serve bupha's read-only CI views, while the tamias verbs
above serve this bundle. tamias finds it on `PATH` or in
`$TAMIAS_HOME/drivers/` — `cargo build --release -p bupha-driver`
produces `target/release/tamias-driver-bupha` (or install the
`libraries/bupha` pack, which ships it as a bundled driver and
deploys the `bupha` `bins[]` command shim — T-012).
