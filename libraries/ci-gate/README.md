# ci-gate — bupha CI gate as a tamias bundle

The org's push-to-verdict CI gate packaged as a tamias content
bundle (`type: bupha`). One watcher (`bupha-watch.service`) fans
GitHub push webhooks into bupha's queue → detached worktree →
per-repo pipeline → commit status, latest-wins per repo.

## Layout

| file | role |
| --- | --- |
| `tamias.yaml` | package manifest — `provider:` declares the systemd unit (`bupha watch --config ~/.config/bupha/bundles/ci-gate.toml`), the env file, and the public ingress (`https://ci.luisfuturist.com` → `127.0.0.1:29173/hook`) plus the GitHub hooks to manage |
| `fleet.toml` | the operator config payload — every `[[repo]]` and the `[runner]`/`[status]`/`[queue]`/`[worktree]` blocks. Deploy merges the manifest-owned `tunnel.public_url`/`trigger.listen`/`trigger.kind` into it and writes `~/.config/bupha/bundles/ci-gate.toml` |

The fleet file is deliberately small and representative — the full
org-repo import is a separate workstream; drop the generated
`[[repo]]` set into `fleet.toml` when it lands.

## Flow

```sh
tamias install <ref>          # fetch + unpack the payload; driver install is a no-op
tamias deploy ci-gate         # render bupha-watch.service AND run the driver's deploy:
                              #   fleet.toml + provider.ingress → ~/.config/bupha/bundles/ci-gate.toml
tamias driver exec ci-gate hooks register --dry-run   # print the webhook plan
tamias driver exec ci-gate hooks register             # upsert GitHub hooks (needs secrets)
tamias service enable bupha-watch                      # start the watcher (tamias-rendered unit)
tamias driver exec ci-gate health                      # ping /hook + unit probe, JSON
tamias driver exec ci-gate status                      # per-unit is-active/is-enabled, JSON
tamias driver exec ci-gate logs --lines 200            # journalctl over the provider units
tamias uninstall ci-gate        # remove the unit; driver uninstall removes bundles/ci-gate.toml
```

`tamias driver config-schema ci-gate` prints the JSON Schema
(draft 2020-12) of the operator config `deploy` writes.

## Secrets

Names only, never values. Resolution order env → OS keyring →
`~/.config/bupha/env` (the unit's `EnvironmentFile`):

- `BUPHA_TOKEN` — forge token for statuses + hook management
  (`[runner] token_env`)
- `BUPHA_WH_<OWNER>_<REPO>` — per-repo webhook secret
  (`[[repo]] secret_env`); the global fallback is
  `BUPHA_WEBHOOK_SECRET` (`[trigger] secret_env`)

`bupha deploy env` (or `tamias-driver-bupha`'s sibling flow) seeds
`~/.config/bupha/env` with the skeleton; fill values there or via
`secret-tool`.

## Driver

`tamias-driver-bupha` (this repo's `crates/bupha-driver`) is a
 dual-surface binary: the pavo verbs (`manifest`/`capabilities`/
`serve`) serve bupha's read-only CI views, while the tamias verbs
above serve this bundle. tamias finds it on `PATH` or in
`$TAMIAS_HOME/drivers/` — `cargo build --release -p bupha-driver`
produces `target/release/tamias-driver-bupha`.
