//! views — the driver's manifest: every ViewSpec plus the query
//! resolvers that feed them.
//!
//! Read path: every query resolves a [`crate::data::Plane`] (params
//! `base`/`config`, else the watcher's own config discovery) and
//! reads `history.jsonl` / `queue.redb` / the systemd units — see
//! `data.rs`. A failed read degrades to an `{"error": …}` or
//! `{"note": …}` payload inside an `ok:true` response, matching the
//! sibling drivers' contract.
//!
//! Surface mapping:
//!
//! | pavo view       | data                                        |
//! |-----------------|---------------------------------------------|
//! | `bupha:status`  | plane digest — base, queue kind, counts, unit states |
//! | `bupha:runs`    | `history.jsonl` tail (`repo`/`limit` params)  |
//! | `bupha:queue`   | `queue.redb` pending/running per key          |
//! | `bupha:repos`   | latest history record per repo/job key        |
//! | `bupha:service` | `systemctl --user` state of watch/worker units |

use std::collections::BTreeMap;

use pavo_driver_kit as kit;
use pavo_protocol::{DriverManifest, TableColumn, ViewNode, ViewSpec};
use serde_json::{json, Map, Value};

use crate::data;

// ---------------------------------------------------------------- helpers
// Node/manifest construction goes through pavo-driver-kit — the
// constructors emit the same ViewNode/QueryBinding structs the
// hand-rolled literals did.

fn col(title: &str, children: Vec<ViewNode>) -> ViewNode {
    kit::column(kit::base().title(title), children)
}

fn kv(title: &str, query: &str) -> ViewNode {
    kit::kv(kit::base().title(title).query(query), Vec::new())
}

fn table(title: &str, query: &str, columns: &[(&str, &str)]) -> ViewNode {
    kit::sortable_table(
        kit::base().title(title).query(query),
        columns
            .iter()
            .map(|(key, t)| kit::table_column(key, t))
            .collect::<Vec<TableColumn>>(),
        Vec::new(),
    )
}

fn spec(
    id: &str,
    title: &str,
    route: &str,
    description: &str,
    queries: &[&str],
    root: ViewNode,
) -> ViewSpec {
    let mut view = kit::view(id, title, route, root).description(description);
    for q in queries {
        view = view.query(kit::binding(q, "query"));
    }
    view.build()
}

// ---------------------------------------------------------------- manifest

const RUN_COLS: &[(&str, &str)] = &[
    ("repo", "Repo"),
    ("sha", "Sha"),
    ("status", "Status"),
    ("wall_secs", "Wall (s)"),
];

const QUEUE_COLS: &[(&str, &str)] = &[
    ("key", "Key"),
    ("pending", "Pending"),
    ("running", "Running"),
    ("pending_shas", "Pending shas"),
];

const UNIT_COLS: &[(&str, &str)] = &[
    ("unit", "Unit"),
    ("enabled", "Enabled"),
    ("active", "Active"),
    ("detail", "Detail"),
];

/// Every view the driver declares — static (no bupha state needed;
/// `manifest`/`views` must answer anywhere).
pub fn specs() -> Vec<ViewSpec> {
    vec![
        spec(
            "status",
            "Status",
            "/bupha/status",
            "Plane digest — resolved worktree base, queue backend, run/queue counts, and watcher unit states.",
            &["summary"],
            col("Bupha status", vec![kv("Plane", "summary")]),
        ),
        spec(
            "runs",
            "Runs",
            "/bupha/runs",
            "Recent finished runs from history.jsonl — repo, sha, outcome, wall time. Filter with --param repo=owner/name (job keys owner/name:job match too); cap with --param limit= (default 100).",
            &["runs"],
            table("Recent runs", "runs", RUN_COLS),
        ),
        spec(
            "queue",
            "Queue",
            "/bupha/queue",
            "Persisted queue state per watched key — pending backlog and the in-flight sha, from queue.redb. Redis-backed fleets degrade to a note (their queue lives in redis).",
            &["queue"],
            table("Queue", "queue", QUEUE_COLS),
        ),
        spec(
            "repos",
            "Repos",
            "/bupha/repos",
            "Latest recorded status per repo/job key — the 'statuses on each sha' surface, folded from run history. Filter with --param repo=.",
            &["repos"],
            table("Latest per repo", "repos", RUN_COLS),
        ),
        spec(
            "service",
            "Service",
            "/bupha/service",
            "systemd user unit state for bupha-watch.service and bupha-worker.service — the same probe `bupha service status` runs.",
            &["units"],
            table("Units", "units", UNIT_COLS),
        ),
    ]
}

/// The `manifest` op result — built with `pavo_driver_kit`'s
/// `Manifest` fluent builder (verbs/capabilities default to
/// `["manifest","serve"]`/`["pavo"]`, matching the literals this
/// replaces).
pub fn manifest() -> DriverManifest {
    let mut navbar = kit::contribution("runs", "CI", "/bupha/runs");
    navbar.priority = 20;
    let mut dashboard = kit::contribution("status", "Bupha status", "/bupha/status");
    dashboard.priority = 30;
    let mut statusbar = kit::contribution("status", "Bupha queue", "/bupha/status");
    statusbar.priority = 40;

    let mut manifest = kit::Manifest::new("bupha", "Bupha")
        .version(env!("CARGO_PKG_VERSION"))
        .icon("🐦")
        .navbar(navbar)
        .dashboard(dashboard)
        .statusbar(statusbar);
    for view in specs() {
        manifest = manifest.view(view);
    }
    manifest.build()
}

// ---------------------------------------------------------------- queries

/// Every query name the driver answers — the `query` op rejects
/// anything outside this set with `ok:false`.
pub fn known_query(name: &str) -> bool {
    matches!(name, "summary" | "runs" | "queue" | "repos" | "units")
}

/// `{"op":"query","query":name,"params":{…}}` → the data payload.
/// Failures degrade to `{"error": …}`/`{"note": …}` objects, never
/// `ok:false`.
pub fn query_data(name: &str, params: &Map<String, Value>) -> Value {
    match name {
        "summary" => summary(params),
        "runs" => runs_rows(params),
        "queue" => queue_rows(params),
        "repos" => repos_rows(params),
        "units" => Value::Array(unit_rows()),
        other => json!({"error": format!("unknown query '{other}'")}),
    }
}

/// Default row cap for `runs`/`repos` — history.jsonl is unbounded;
/// `--param limit=` overrides.
const DEFAULT_LIMIT: u64 = 100;

/// `summary` query — the status view's kv object: resolved plane
/// plus live counts. Sub-read failures degrade to their message
/// rather than failing the panel.
fn summary(ctx: &Map<String, Value>) -> Value {
    let plane = data::resolve(ctx);
    let history = data::history_path(&plane.base);
    let queue = match data::queue_entries(&plane.base, &plane.queue_kind) {
        Ok(entries) => format!(
            "{} key(s), {} pending",
            entries.len(),
            data::pending_total(&entries)
        ),
        Err(e) => e,
    };
    let mut out = Map::new();
    out.insert("base".into(), json!(plane.base));
    out.insert("queueKind".into(), json!(plane.queue_kind));
    out.insert(
        "history".into(),
        json!(if history.exists() {
            "present"
        } else {
            "absent"
        }),
    );
    out.insert("queue".into(), json!(queue));
    for (unit, status) in data::unit_statuses() {
        out.insert(unit.trim_end_matches(".service").to_string(), json!(status));
    }
    Value::Object(out)
}

/// One `history.jsonl` record → table row. Pure — unit-tested with
/// synthetic JSON.
fn run_row(r: &Value) -> Value {
    json!({
        "repo": r["repo"],
        "sha": r["sha"],
        "status": r["status"],
        "wall_secs": r["wall_secs"],
    })
}

/// `runs` query — the tail of `history.jsonl`, newest last.
fn runs_rows(ctx: &Map<String, Value>) -> Value {
    let plane = data::resolve(ctx);
    let repo = data::param_str(ctx, "repo");
    let limit = data::param_u64(ctx, "limit").unwrap_or(DEFAULT_LIMIT) as usize;
    Value::Array(
        data::run_records(&plane.base, repo, limit)
            .iter()
            .map(run_row)
            .collect(),
    )
}

/// Short sha for the pending-shas preview column.
fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// One queue entry → table row. `pending_shas` previews up to five
/// pending shas (short form); the count column stays exact.
fn queue_row(key: &str, q: &bupha_queue::PersistedQueue) -> Value {
    let shas: Vec<&str> = q.pending.iter().map(|s| short_sha(s)).collect();
    let preview = if shas.len() > 5 {
        format!("{} …", shas[..5].join(" "))
    } else {
        shas.join(" ")
    };
    json!({
        "key": key,
        "pending": q.pending.len(),
        "running": q.running.as_deref().map(short_sha).unwrap_or("—"),
        "pending_shas": preview,
    })
}

/// `queue` query — persisted pending/running per key. Non-redb
/// queue kinds degrade to a note (redis fleet state is unreadable
/// here); a missing store reads as an empty queue.
fn queue_rows(ctx: &Map<String, Value>) -> Value {
    let plane = data::resolve(ctx);
    match data::queue_entries(&plane.base, &plane.queue_kind) {
        Ok(entries) => Value::Array(entries.iter().map(|(k, q)| queue_row(k, q)).collect()),
        Err(e) => json!({"note": e}),
    }
}

/// Latest history record per repo/job key — history is append-only,
/// so the last write wins. Returns rows in repo-key order.
fn latest_by_repo(records: &[Value]) -> Vec<Value> {
    let mut latest: BTreeMap<String, &Value> = BTreeMap::new();
    for r in records {
        if let Some(repo) = r["repo"].as_str() {
            latest.insert(repo.to_string(), r);
        }
    }
    latest.values().map(|r| run_row(r)).collect()
}

/// `repos` query — per-key latest status (the "statuses on each
/// sha" surface), optionally repo-filtered.
fn repos_rows(ctx: &Map<String, Value>) -> Value {
    let plane = data::resolve(ctx);
    let repo = data::param_str(ctx, "repo");
    // Latest-per-key is bounded by the number of watched keys, not
    // history length — `limit` doesn't apply here.
    let records = data::run_records(&plane.base, repo, usize::MAX);
    Value::Array(latest_by_repo(&records))
}

/// `units` query — systemd state rows for the watcher units. The
/// status line is `enabled=<state> active=<state>`; unparseable
/// probes leave the fields at "?" with the raw line in `detail`.
fn unit_rows() -> Vec<Value> {
    data::unit_statuses()
        .into_iter()
        .map(|(unit, s)| {
            let mut enabled = "?";
            let mut active = "?";
            let mut parsed = false;
            for tok in s.split_whitespace() {
                if let Some(v) = tok.strip_prefix("enabled=") {
                    enabled = v;
                    parsed = true;
                } else if let Some(v) = tok.strip_prefix("active=") {
                    active = v;
                    parsed = true;
                }
            }
            json!({
                "unit": unit,
                "enabled": enabled,
                "active": active,
                "detail": if parsed { "" } else { s.as_str() },
            })
        })
        .collect()
}

// ---------------------------------------------------------------- view op

/// `{"op":"view","view":id,"params":{…}}` → [`pavo_protocol::wire::ViewResult`].
pub fn view_result(
    view_id: &str,
    params: &Map<String, Value>,
) -> Result<pavo_protocol::wire::ViewResult, String> {
    let spec = specs()
        .into_iter()
        .find(|v| v.id == view_id)
        .ok_or_else(|| format!("unknown view: {view_id}"))?;

    let mut data_map = Map::new();
    for b in &spec.queries {
        data_map.insert(b.name.clone(), query_data(&b.name, params));
    }

    Ok(pavo_protocol::wire::ViewResult {
        view: spec,
        data: data_map,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::data::tests_util::tmp;
    use serde_json::json;
    use std::collections::VecDeque;

    fn ctx_base(dir: &std::path::Path) -> Map<String, Value> {
        [("base".to_string(), json!(dir.to_string_lossy()))]
            .into_iter()
            .collect()
    }

    fn write_history(dir: &std::path::Path, lines: &[&str]) {
        let body = lines.join("\n") + "\n";
        std::fs::write(dir.join("history.jsonl"), body).unwrap();
    }

    #[test]
    fn manifest_advertises_pavo_and_lists_every_view() {
        let m = manifest();
        assert!(m.verbs.iter().any(|v| v == "serve"));
        assert!(m.capabilities.iter().any(|c| c == "pavo"));
        assert!(m.renderers.is_empty());
        let ids: std::collections::BTreeSet<&str> = m.views.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids.len(), m.views.len(), "duplicate view ids");
        for want in ["status", "runs", "queue", "repos", "service"] {
            assert!(ids.contains(want), "missing view {want}");
        }
        assert!(m
            .contributes
            .navbar
            .iter()
            .any(|c| c.route == "/bupha/runs"));
    }

    #[test]
    fn every_binding_is_a_known_query() {
        for v in specs() {
            for b in &v.queries {
                assert!(known_query(&b.name), "{}: unknown query {}", v.id, b.name);
            }
        }
        assert!(!known_query("bogus"));
    }

    #[test]
    fn run_row_maps_fields() {
        let row = run_row(&json!({
            "sha": "abc123", "repo": "o/r", "status": "success", "wall_secs": 42,
        }));
        assert_eq!(row["repo"], "o/r");
        assert_eq!(row["wall_secs"], 42);
    }

    #[test]
    fn short_sha_branches() {
        assert_eq!(short_sha("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_sha("short"), "short");
    }

    #[test]
    fn queue_row_preview_and_defaults() {
        use bupha_queue::PersistedQueue;
        let q = PersistedQueue {
            pending: VecDeque::from([
                "aaaaaaaaaaaa1111".to_string(),
                "bbbbbbbbbbbb2222".to_string(),
                "cccccccccccc3333".to_string(),
                "dddddddddddd4444".to_string(),
                "eeeeeeeeeeee5555".to_string(),
                "ffffffffffff6666".to_string(),
            ]),
            running: None,
        };
        let row = queue_row("o/r", &q);
        assert_eq!(row["pending"], 6);
        assert_eq!(row["running"], "—");
        let preview = row["pending_shas"].as_str().unwrap();
        assert!(preview.contains('…'));
        assert!(preview.starts_with("aaaaaaaaaaaa"));

        let q2 = PersistedQueue {
            pending: VecDeque::from(["0123456789abcdef".to_string()]),
            running: Some("fedcba9876543210".to_string()),
        };
        let row2 = queue_row("o/r:gate", &q2);
        assert_eq!(row2["pending"], 1);
        assert_eq!(row2["running"], "fedcba987654");
        assert_eq!(row2["pending_shas"], "0123456789ab");
    }

    #[test]
    fn latest_by_repo_last_write_wins() {
        let records = vec![
            json!({"repo": "o/r", "sha": "old", "status": "failure"}),
            json!({"repo": "o/r:gate", "sha": "g1", "status": "success"}),
            json!({"repo": "o/r", "sha": "new", "status": "success"}),
            json!({"sha": "norepo"}), // skipped — no repo key
        ];
        let rows = latest_by_repo(&records);
        assert_eq!(rows.len(), 2);
        let r = rows.iter().find(|r| r["repo"] == "o/r").unwrap();
        assert_eq!(r["sha"], "new");
        let g = rows.iter().find(|r| r["repo"] == "o/r:gate").unwrap();
        assert_eq!(g["sha"], "g1");
    }

    #[test]
    fn runs_rows_reads_history_with_filter_and_limit() {
        let d = tmp("v-runs");
        write_history(
            &d,
            &[
                "{\"sha\":\"s1\",\"repo\":\"o/r\",\"status\":\"success\",\"wall_secs\":1}",
                "{\"sha\":\"s2\",\"repo\":\"o/r:gate\",\"status\":\"failure\",\"wall_secs\":2}",
                "{\"sha\":\"s3\",\"repo\":\"o/r\",\"status\":\"cancelled\",\"wall_secs\":3}",
            ],
        );
        let mut c = ctx_base(&d);
        c.insert("repo".into(), json!("o/r"));
        c.insert("limit".into(), json!(1));
        let v = runs_rows(&c);
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["sha"], "s3"); // limit keeps the tail
                                          // no filter → all three
        let all = runs_rows(&ctx_base(&d));
        assert_eq!(all.as_array().unwrap().len(), 3);
    }

    #[test]
    fn repos_rows_latest_per_key() {
        let d = tmp("v-repos");
        write_history(
            &d,
            &[
                "{\"sha\":\"s1\",\"repo\":\"o/r\",\"status\":\"failure\",\"wall_secs\":1}",
                "{\"sha\":\"s2\",\"repo\":\"o/r\",\"status\":\"success\",\"wall_secs\":2}",
                "{\"sha\":\"s3\",\"repo\":\"o/x\",\"status\":\"success\",\"wall_secs\":3}",
            ],
        );
        let v = repos_rows(&ctx_base(&d));
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let r = rows.iter().find(|r| r["repo"] == "o/r").unwrap();
        assert_eq!(r["sha"], "s2");
        // repo filter narrows to the one key
        let mut c = ctx_base(&d);
        c.insert("repo".into(), json!("o/x"));
        let one = repos_rows(&c);
        assert_eq!(one.as_array().unwrap().len(), 1);
        assert_eq!(one[0]["repo"], "o/x");
    }

    #[test]
    fn queue_rows_reads_store_and_notes_redis() {
        use bupha_queue::{PersistedQueue, QueueStore};
        use bupha_queue_redb::RedbQueueStore;
        let d = tmp("v-queue");
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "o/r".to_string(),
            PersistedQueue {
                pending: VecDeque::from(["a".repeat(40)]),
                running: Some("b".repeat(40)),
            },
        );
        let store = RedbQueueStore::open(d.join("queue.redb")).unwrap();
        store.persist(&map).unwrap();
        drop(store);
        let v = queue_rows(&ctx_base(&d));
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["key"], "o/r");
        assert_eq!(rows[0]["pending"], 1);

        // redis kind → note payload, not an error
        let cfg = d.join("bupha.toml");
        std::fs::write(
            &cfg,
            "[runner]\nkind = \"shell\"\n[status]\nkind = \"none\"\n[queue]\nkind = \"redis\"\n",
        )
        .unwrap();
        let mut c = Map::new();
        c.insert("config".into(), json!(cfg.to_string_lossy()));
        let note = queue_rows(&c);
        assert!(note["note"].as_str().unwrap().contains("redis"));
    }

    #[test]
    fn summary_reports_plane_and_degrades_queue() {
        let d = tmp("v-summary");
        write_history(
            &d,
            &["{\"sha\":\"s\",\"repo\":\"o/r\",\"status\":\"success\",\"wall_secs\":1}"],
        );
        let v = summary(&ctx_base(&d));
        assert_eq!(v["base"], json!(d.to_string_lossy()));
        assert_eq!(v["history"], "present");
        assert!(v["queue"].as_str().unwrap().contains("0 pending"));
        assert!(v["bupha-watch"].is_string());
        assert!(v["bupha-worker"].is_string());

        // absent history + redis queue degrade to strings/notes
        let empty = tmp("v-summary-empty");
        let cfg = empty.join("bupha.toml");
        std::fs::write(
            &cfg,
            "[runner]\nkind = \"shell\"\n[status]\nkind = \"none\"\n[queue]\nkind = \"redis\"\n[worktree]\nbase = \"/nonexistent\"\n",
        )
        .unwrap();
        let mut c = Map::new();
        c.insert("config".into(), json!(cfg.to_string_lossy()));
        let v2 = summary(&c);
        assert_eq!(v2["history"], "absent");
        assert!(v2["queue"].as_str().unwrap().contains("redis"));
    }

    #[test]
    fn units_rows_parse_status_line() {
        let rows = unit_rows();
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert!(r["unit"].as_str().unwrap().contains("bupha-"));
            // "enabled=… active=…" parses; anything else lands in detail
            assert!(r["enabled"].is_string());
            assert!(r["active"].is_string());
        }
    }

    #[test]
    fn query_data_unknown_name_is_error_payload() {
        let v = query_data("bogus", &Map::new());
        assert!(v["error"].as_str().unwrap().contains("bogus"));
    }

    #[test]
    fn view_result_unknown_view_errors() {
        assert!(view_result("nope", &Map::new()).is_err());
        let ok = view_result("runs", &Map::new()).unwrap();
        assert_eq!(ok.view.id, "runs");
        assert!(ok.data.contains_key("runs"));
    }
}
