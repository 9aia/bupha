//! bupha-driver — library half of `tamias-driver-bupha`, the pavo
//! view driver for bupha's CI state.
//!
//! Verbs (the tamias driver contract, served by `src/main.rs`):
//!
//! - `manifest` — print the [`pavo_protocol::DriverManifest`] JSON.
//! - `capabilities` — `{"capabilities": ["pavo"], "verbs": […]}`.
//! - `serve` — ndjson JSON-RPC on stdio: `{"op":"manifest"}`,
//!   `{"op":"views"}`, `{"op":"view","view":…,"params":{…}}`,
//!   `{"op":"query","query":…,"params":{…}}` →
//!   `{"ok":true,"result":…}` / `{"ok":false,"error":…}`. The loop
//!   itself is `pavo_driver_kit::serve_stdio`; `render`/`submit`/
//!   `jobs`/`notifications` answer `ok:false` — this driver claims
//!   none of them.
//!
//! Engine ops (the "CLI-as-generated-surface" contract, P-021):
//! `{"op":"query","query":"ops"}` answers the ops manifest —
//! every bounded `bupha` CLI verb derived from the engine's own
//! clap tree ([`engine_ops`]); `query:"cli.<verb>"` re-execs
//! `tamias-driver-bupha run <argv>` in a captured child and
//! returns `{op, argv, code, stdout, stderr}`. Unbounded daemons
//! (`watch`/`agent`/`gha-runner`) ride `surfaces`.
//!
//! Read path: bupha keeps no serve wire — its state is files under
//! `worktree.base` (`history.jsonl`, `queue.json`, plus the systemd
//! user units the watcher manages). The driver reads those
//! directly through the same crates the watcher uses
//! (`bupha-config-file` for config resolution, `bupha-queue`
//! for the queue store, `bupha-systemd` for unit state) — the most
//! stable read path available; the `bupha` CLI's `runs`/`log` verbs
//! print human text with no `--json`, so proxying them would mean
//! parsing prose. See `data.rs`.

pub mod data;
pub mod engine_ops;
pub mod ops;
#[cfg(any(feature = "cli", feature = "mcp"))]
pub mod surface;
pub mod views;

use pavo_driver_kit as kit;
use pavo_protocol::wire::{self, Request, Response, ViewSummary};
use serde_json::Value;

/// `manifest` verb payload — the manifest as a JSON value.
pub fn manifest_value() -> Value {
    serde_json::to_value(views::manifest()).unwrap_or(Value::Null)
}

/// `capabilities` verb payload — merged for both consumers of this
/// binary. pavo's discovery checks `capabilities`/`verbs` for
/// `"pavo"`/`"serve"`; tamias's checks `types` for the content kind
/// and warns on undeclared `verbs`. One JSON answers both: the kit's
/// `{capabilities, verbs}` shape plus `"types": ["bupha"]` and the
/// tamias provider verbs folded into `verbs`.
pub fn capabilities_value() -> Value {
    let m = views::manifest();
    let mut v = kit::capabilities_value(&m);
    v["views"] = serde_json::json!(m.views.len());
    v["types"] = serde_json::json!(["bupha"]);
    let mut verbs: Vec<Value> = v["verbs"].as_array().cloned().unwrap_or_default();
    for extra in [
        "capabilities",
        "install",
        "deploy",
        "uninstall",
        "hooks",
        "health",
        "status",
        "logs",
        "host",
        "worker",
        "config-schema",
        "ops",
        "run",
    ] {
        if !verbs.iter().any(|x| x.as_str() == Some(extra)) {
            verbs.push(serde_json::json!(extra));
        }
    }
    // Feature-gated embedded surfaces — advertised only when the
    // binary actually answers them.
    #[cfg(feature = "cli")]
    if !verbs.iter().any(|x| x.as_str() == Some("cli")) {
        verbs.push(serde_json::json!("cli"));
    }
    #[cfg(feature = "mcp")]
    if !verbs.iter().any(|x| x.as_str() == Some("mcp")) {
        verbs.push(serde_json::json!("mcp"));
    }
    v["verbs"] = Value::Array(verbs);
    v
}

/// Dispatch one parsed request to its response.
pub fn dispatch(req: &Request) -> Response {
    match req {
        Request::Manifest => Response::ok_typed(&views::manifest()),
        Request::Views => {
            let summaries: Vec<ViewSummary> =
                views::specs().iter().map(ViewSummary::from).collect();
            Response::ok_typed(&summaries)
        }
        Request::View { view, params } => match views::view_result(view, params) {
            Ok(result) => Response::ok_typed(&result),
            Err(e) => Response::err(e),
        },
        Request::Query { query, params } => {
            // `ops` and `cli.<verb>` are engine ops; everything
            // else is a view query — unknown names fail the op,
            // degraded reads travel as `{"error": …}` /
            // `{"note": …}` data payloads, matching the sibling
            // drivers' contract.
            if query == "ops" {
                return Response::ok_typed(&wire::QueryResult {
                    query: query.clone(),
                    data: engine_ops::manifest(),
                });
            }
            if let Some(op) = engine_ops::op_of(query) {
                return engine_ops::run_verb(&op, params);
            }
            if !views::known_query(query) {
                return Response::err(format!("unknown query: {query}"));
            }
            let payload = views::query_data(query, params);
            Response::ok_typed(&wire::QueryResult {
                query: query.clone(),
                data: payload,
            })
        }
        // `render`/`submit` — this driver ships no renderer plugins
        // and no forms; the manifest claims neither, so answering an
        // explicit error keeps the wire honest.
        Request::Render { kind, .. } => Response::err(format!(
            "tamias-driver-bupha claims no renderers (kind '{kind}')"
        )),
        Request::Submit { .. } => {
            Response::err("tamias-driver-bupha is read-only and implements no forms")
        }
        // Optional platform feeds — the manifest declares neither
        // `contributes.jobs` nor `contributes.notifications`, so per
        // convention the ops answer `ok:false` ("no feed").
        Request::Jobs => Response::err("tamias-driver-bupha has no job feed"),
        Request::Notifications => Response::err("tamias-driver-bupha has no notification feed"),
    }
}

/// The `serve` verb — ndjson JSON-RPC on stdio until EOF or a
/// broken pipe, via pavo-driver-kit's loop (same semantics as the
/// hand-rolled loop this replaces: blank lines skip, parse failures
/// answer `ok:false`, a dead client exits quietly).
pub fn serve() -> i32 {
    // The kit reports only unexpected I/O errors; the historical
    // contract is exit 0 on EOF/EPIPE either way.
    let _ = kit::serve_stdio(dispatch);
    0
}
