//! tamias-driver-bupha — dual-surface driver for bupha's CI state.
//!
//! One binary, two consumers, disjoint argv:
//!
//! **pavo view driver** (verbs take no `--dir`/`--name`):
//!
//! - `manifest` — print the [`pavo_protocol::DriverManifest`] JSON
//!   (discovery probe; never touches bupha state).
//! - `capabilities` — merged JSON advertising BOTH surfaces: pavo's
//!   `{"capabilities": ["pavo"], "verbs": […"serve"…]}` shape plus
//!   tamias's `"types": ["bupha"]` and the provider verbs in `verbs`.
//!   pavo's discovery greps `capabilities`/`verbs` for
//!   `"pavo"`/`"serve"`; tamias's greps `types` for the content kind
//!   and warns on verbs missing from `verbs` — one payload satisfies
//!   both.
//! - `serve` — ndjson JSON-RPC on stdio (see `bupha_driver::serve`).
//!
//! **tamias content-type driver** (`type: bupha` packages — tamias
//! always passes `--dir <payload> --name <pkg> [--config <f>]`, so
//! these verbs are unreachable from pavo's bare probes):
//!
//! - `install` — no-op (the operator config is `deploy`'s job; install
//!   only sees the evictable fetch-cache payload).
//! - `host` — engine-host mode: run the watcher pipeline
//!   (`bupha-watch`) in-process against the deployed operator
//!   config — what provider units exec (`tamias-driver-bupha host
//!   --config ~/.config/bupha/bundles/<name>.toml`). Blocks until
//!   the sources stop; `--dir`/`--name` accepted per the tamias
//!   dispatch contract (`--name` supplies the default
//!   `bundles/<name>.toml` path when `--config` is absent).
//!   **Standalone/dev mode** (ADR-0007): the trigger listener this
//!   hosts is the fallback ingress — production webhook ingress is
//!   a peregrin campaign listener enqueuing `ci-job` via
//!   `ci.enqueue`.
//! - `deploy` — merge the payload's `fleet.toml` with
//!   `tamias.yaml provider.ingress[]` (`tunnel.public_url`,
//!   `trigger.listen`/`kind`) and write
//!   `~/.config/bupha/bundles/<name>.toml` — the file the provider
//!   unit's `host` verb consumes.
//! - `uninstall` — remove that file.
//! - `hooks register|uninstall [--dry-run] [--api-base <url>]
//!   [--token-env <NAME>]` — upsert/remove the GitHub webhooks
//!   `provider.ingress[].hooks[]` declare, via `bupha_github`'s REST
//!   path. Secrets resolve by NAME (env → cuora → keyring →
//!   `~/.config/bupha/env`; cuora canonical per T-056); values are
//!   never printed.
//!   **Standalone mode** (ADR-0007): registers hooks at the bupha
//!   listener — production ingress is peregrin campaigns, so use
//!   this only for deployments with no peregrin.
//! - `health` — JSON probe: webhook `ping` per ingress
//!   `listen`+`path`, else `systemctl --user is-active` per unit.
//! - `status` — JSON per-unit `is-active`/`is-enabled`.
//! - `logs [--lines N]` — `journalctl --user` over the provider units.
//! - `config-schema` — JSON Schema (draft 2020-12) of the operator
//!   config `deploy` writes (tamias's reserved verb).
//!
//! See `bupha_driver::ops` for the implementation and
//! `bupha_driver::data` for the pavo read plane.

use bupha_driver as drv;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("manifest") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&drv::manifest_value()).unwrap_or_default()
            );
        }
        Some("capabilities") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&drv::capabilities_value()).unwrap_or_default()
            );
        }
        Some("serve") => std::process::exit(drv::serve()),
        // Tamias verbs — always invoked with `--dir`/`--name` by the
        // tamias dispatch contract; pavo never passes those flags, so
        // subcommand-first dispatch keeps the surfaces disjoint.
        Some(
            verb @ ("install" | "deploy" | "uninstall" | "hooks" | "health" | "status" | "logs"
            | "host" | "config-schema"),
        ) => {
            std::process::exit(drv::ops::run(verb, &args.collect::<Vec<_>>()));
        }
        _ => {
            eprintln!("usage: tamias-driver-bupha manifest|capabilities|serve");
            eprintln!("       tamias-driver-bupha <install|deploy|uninstall|hooks|health|status|logs|host|config-schema> --dir <d> --name <n> [--config <c>]");
            std::process::exit(1);
        }
    }
}
