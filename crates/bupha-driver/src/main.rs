//! tamias-driver-bupha — the pavo view driver for bupha's CI state.
//!
//! Verbs (the tamias driver contract):
//!
//! - `manifest` — print the [`pavo_protocol::DriverManifest`] JSON
//!   (discovery probe; never touches bupha state).
//! - `capabilities` — `{"capabilities": ["pavo"], "verbs": […]}`.
//! - `serve` — ndjson JSON-RPC on stdio (see `bupha_driver::serve`).
//!
//! Read path: files under `worktree.base` — see `bupha_driver::data`
//! for resolution (`--param base=`/`config=`, `$BUPHA_CONFIG`,
//! `./bupha.*`, `~/.config/bupha/`, default `/tmp/bupha`). The driver
//! is strictly read-only: it never writes queue/history/audit state
//! and never invokes mutating `bupha` verbs.

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
        _ => {
            eprintln!("usage: tamias-driver-bupha manifest|capabilities|serve");
            std::process::exit(1);
        }
    }
}
