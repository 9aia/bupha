# How-to: package bupha fleets with tamias

A bupha fleet — watcher unit + operator config + webhook wiring —
ships as a tamias content bundle (`type: bupha`). tamias owns the
package lifecycle (fetch, install store, provider units, hooks);
bupha owns what runs inside. The shipped examples live in
`libraries/`: `ci-gate` (push-to-verdict gate on `main`) and
`docs-deploy` (push to `docs` → site deploy → `docs/deploy`
status).

## The `type: bupha` contract

Two files make a bundle:

- **`tamias.yaml`** — the package manifest. `provider:` declares
  the systemd unit(s) (`<provider.name>-<service>`), the env file,
  and `ingress[]` — the public URL, listen address, and the
  GitHub hooks to manage. `verbs:` is advisory for
  `tamias provider list`/`tamias run`.
- **`fleet.toml`** — a *complete* `bupha_core::Config` doc: the
  `[[repo]]` set plus `[runner]`/`[status]`/`[queue]`/`[worktree]`
  blocks. `tamias-driver-bupha deploy` merges the manifest-owned
  fields (`tunnel.public_url`, `trigger.listen`,
  `trigger.kind`) into it and writes
  `~/.config/bupha/bundles/<name>.toml` — the config the provider
  unit's `tamias-driver-bupha host --config` actually runs.

Secrets are env-var **names** everywhere (`secret_env`,
`token_env`); values resolve env → **cuora**
(`cuora://bupha/<NAME>` via the `cuora` CLI — canonical, T-056) →
OS keyring → `~/.config/bupha/env` (the unit's `EnvironmentFile`,
legacy fallback). Seed with `printf '%s' "$VALUE" | cuora put
cuora://bupha/<NAME>`. Never a value in either file.

> **Ingress note (ADR-0007).** The `provider.ingress` block +
> `hooks register` wire the bupha webhook listener — the
> **standalone/dev** ingress for deployments with no peregrin.
> Production webhook/forge ingress is a peregrin campaign
> listener that normalizes deliveries through bupha's `Trigger`
> semantics and enqueues `ci-job` via `ci.enqueue`.

## Install → deploy → hooks

```sh
tamias install <ref>            # fetch + unpack; driver install is a no-op
tamias deploy <bundle>          # render the provider unit AND write
                                # bundles/<name>.toml from fleet.toml + ingress
tamias driver exec <bundle> hooks register --dry-run   # webhook plan
tamias driver exec <bundle> hooks register             # upsert GitHub hooks
tamias service enable <unit>   # start the watcher (e.g. bupha-watch-docs)
tamias uninstall <bundle>      # remove unit + bundles/<name>.toml
```

`tamias driver exec <bundle>` verbs: `hooks register`
(create/update the declared forge hooks), `health` (ping `/hook`
+ unit probe, JSON), `status` (per-unit is-active/is-enabled),
`logs` (journalctl over the provider units). `tamias driver
config-schema <bundle>` prints the JSON Schema of the operator
config `deploy` writes.

The driver — `tamias-driver-bupha`, built from
`crates/bupha-driver` — is dual-surface: the tamias verbs above
plus the pavo view verbs (`manifest`/`capabilities`/`serve`)
that serve bupha's read-only CI views. tamias finds it on `PATH`
or in `$TAMIAS_HOME/drivers/`.

## Writing your own bundle

1. Copy `libraries/docs-deploy/` — it's the smaller template.
2. Edit `fleet.toml`: one `[[repo]]` per watched repo
   (`name`/`dir`/`branch`; `events = ["push", "pull_request"]`
   opts into PR deliveries), the `[runner]` your pipelines need
   (`shell` + `cmd`, `gha` + `workflow`, `dagger`, …), and
   `[status].context` for the commit-status label.
3. Edit `tamias.yaml`: `provider.services[]` exec should be
   `tamias-driver-bupha host --config ~/.config/bupha/bundles/<name>.toml`
   (the driver binary from `$TAMIAS_HOME/drivers/` or `PATH` — the
   `host` verb runs the watcher engine in-process);
   pick a **unique service name** — the unit is
   `<provider.name>-<service>`, and two bundles sharing
   `bupha-watch.service` would overwrite each other (ci-gate owns
   `watch`, docs-deploy owns `watch-docs`). Give `ingress[]` a
   distinct `listen` port and `public_url`, and one `hooks[]`
   entry per `[[repo]]`.
4. `bupha runs --config fleet.toml` (dev CLI) smoke-checks the
   payload parses as a `Config` before you publish.

Two watchers can watch the same repo: GitHub delivers every push
to both hook URLs and each fleet's `branch` filter keeps only its
own refs — `main` for the gate, `docs` for deploys. Overlap costs
a filtered delivery, not a double run.

Coming from a peregrin campaign instead? The dev CLI's
`bupha import <campaign.yaml>` translates the github-webhook +
git-job + github-status shape into a `bupha.yaml` fleet config —
a head start for the `fleet.toml` inside step 2.
