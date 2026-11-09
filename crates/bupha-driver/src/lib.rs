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
//!   `{"ok":true,"result":…}` / `{"ok":false,"error":…}`.
//!
//! Read path: bupha keeps no serve wire — its state is files under
//! `worktree.base` (`history.jsonl`, `queue.redb`, plus the systemd
//! user units `bupha service status` probes). The driver reads those
//! directly through the same crates the watcher uses
//! (`bupha-config-file` for config resolution, `bupha-queue-redb`
//! for the queue store, `bupha-systemd` for unit state) — the most
//! stable read path available; the `bupha` CLI's `runs`/`log` verbs
//! print human text with no `--json`, so proxying them would mean
//! parsing prose. See `data.rs`.

pub mod data;
pub mod views;

use std::io::{BufRead, Write};

use pavo_protocol::wire::{self, Request, Response, ViewSummary};

/// `manifest` verb payload — the manifest as a JSON value.
pub fn manifest_value() -> serde_json::Value {
    serde_json::to_value(views::manifest()).unwrap_or(serde_json::Value::Null)
}

/// `capabilities` verb payload.
pub fn capabilities_value() -> serde_json::Value {
    let m = views::manifest();
    serde_json::json!({
        "capabilities": m.capabilities,
        "verbs": m.verbs,
        "views": m.views.len(),
    })
}

/// Dispatch one parsed request to its response.
pub fn dispatch(req: Request) -> Response {
    match req {
        Request::Manifest => Response::ok_typed(&views::manifest()),
        Request::Views => {
            let summaries: Vec<ViewSummary> =
                views::specs().iter().map(ViewSummary::from).collect();
            Response::ok_typed(&summaries)
        }
        Request::View { view, params } => match views::view_result(&view, &params) {
            Ok(result) => Response::ok_typed(&result),
            Err(e) => Response::err(e),
        },
        Request::Query { query, params } => {
            // Unknown names fail the op; everything else answers
            // `ok:true` — degraded reads travel as `{"error": …}` /
            // `{"note": …}` data payloads, matching the sibling
            // drivers' contract.
            if !views::known_query(&query) {
                return Response::err(format!("unknown query: {query}"));
            }
            let payload = views::query_data(&query, &params);
            Response::ok_typed(&wire::QueryResult {
                query,
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
    }
}

/// The `serve` verb — ndjson JSON-RPC on stdio until EOF or a
/// broken pipe.
pub fn serve() -> i32 {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match wire::parse_request(&line) {
            Ok(req) => dispatch(req),
            // Malformed JSON or an unknown op — the conformance
            // contract wants `ok:false`, never a dropped line.
            Err(e) => Response::err(e.to_string()),
        };
        let text = wire::response_line(&response).unwrap_or_else(|_| {
            "{\"ok\":false,\"error\":\"response serialize failed\"}".to_string()
        });
        // Host went away (closed stdout) — exit quietly. One `break`
        // covers both write and flush failure.
        if writeln!(out, "{text}").is_err() || out.flush().is_err() {
            break;
        }
    }
    0
}
