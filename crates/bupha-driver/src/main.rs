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
//!   `query:"ops"` answers the engine ops manifest; `query:"cli.<verb>"`
//!   runs a bounded `bupha` CLI verb in a captured child.
//!
//! Engine surface (P-021 — the `bupha` CLI as a generated surface):
//!
//! - `ops` — the ops manifest: every bounded `bupha` verb as a
//!   `cli.<verb>` wire query, derived from the engine's own clap
//!   tree (`bupha::command`). Unbounded daemons (`watch`, `agent`,
//!   `gha-runner`) are `surfaces`, never ops.
//! - `run <argv>` — engine passthrough: `tamias-driver-bupha run
//!   doctor --json` ≡ `bupha doctor --json`. Also the child side of
//!   `cli.*` wire queries (`engine_ops::run_verb` re-execs this same
//!   verb so engine output never touches the serve stream).
//!
//! Optional surfaces: features `cli`/`mcp` embed pavo-driver-kit's
//! surface layer — `cli <op> [args]` is a generated CLI over the ops
//! manifest (`watch`/`agent`/`gha-runner` ride as streaming
//! subcommands), `mcp` serves the ops as MCP tools on stdio. Both
//! are equivalent to the standalone `tamias-driver-cli`/`tamias-driver-mcp`
//! surface drivers, minus the PATH/discovery hop.
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
//! - `worker` — the `ci-job` consumption loop (P-002 §4.1): polls
//!   `kind="ci-job"` entities on a hypha/peregrin node
//!   (`[worker] node` / `--node` / `BUPHA_WORKER_NODE`, credential
//!   via `[worker] node_token_env` / `--node-token-env` /
//!   `BUPHA_NODE_TOKEN`), `ci.claim`s pending jobs this box's
//!   `[worker] labels` cover, runs them through the same
//!   `bupha-watch` pipeline, `ci.complete`s the result. The
//!   **second service** a fleet deployment runs — worker boxes
//!   execute jobs the entity plane hands out.
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
#[cfg(any(feature = "cli", feature = "mcp"))]
use pavo_driver_kit as kit;

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
        // Engine ops surface — `ops` prints the manifest derived
        // from `bupha::command()`; `run` is the engine passthrough
        // and the child side of `cli.*` wire queries.
        Some("ops") => {
            println!("{}", drv::engine_ops::manifest());
        }
        Some("run") => {
            let argv = std::iter::once("bupha".to_string()).chain(args);
            std::process::exit(bupha::run_from(argv));
        }
        // Embedded surface verbs (feature-gated, optional): the
        // pavo-driver-kit surface layer renders this driver's own
        // ops manifest — `cli` = generated clap CLI (op → subcommand,
        // `cli watch` streams), `mcp` = ops → MCP tools over stdio.
        // The standalone tamias-driver-cli/-mcp binaries do the same
        // over process dispatch; these verbs need nothing on PATH.
        #[cfg(feature = "cli")]
        Some("cli") => {
            let m = kit::surface::OpsManifest::from_value(&drv::engine_ops::manifest())
                .unwrap_or_default();
            let argv: Vec<String> = std::iter::once("bupha".to_string()).chain(args).collect();
            std::process::exit(kit::surface::cli::run(
                "bupha",
                &m,
                &argv,
                &mut drv::surface::Engine,
            ));
        }
        #[cfg(feature = "mcp")]
        Some("mcp") => {
            let m = kit::surface::OpsManifest::from_value(&drv::engine_ops::manifest())
                .unwrap_or_default();
            match kit::surface::mcp::serve_stdio(&m, &mut drv::surface::Engine) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("tamias-driver-bupha mcp: {e}");
                    std::process::exit(1);
                }
            }
        }
        // Tamias verbs — always invoked with `--dir`/`--name` by the
        // tamias dispatch contract; pavo never passes those flags, so
        // subcommand-first dispatch keeps the surfaces disjoint.
        Some(
            verb @ ("install" | "deploy" | "uninstall" | "hooks" | "health" | "status" | "logs"
            | "host" | "worker" | "config-schema"),
        ) => {
            std::process::exit(drv::ops::run(verb, &args.collect::<Vec<_>>()));
        }
        _ => {
            eprintln!("usage: tamias-driver-bupha manifest|capabilities|ops|run <argv>|serve");
            eprintln!("       (feature `cli` adds `cli <op> [args]`; feature `mcp` adds `mcp`)");
            eprintln!("       tamias-driver-bupha <install|deploy|uninstall|hooks|health|status|logs|host|worker|config-schema> --dir <d> --name <n> [--config <c>]");
            std::process::exit(1);
        }
    }
}
