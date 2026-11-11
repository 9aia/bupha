//! Engine ops — `cli.<verb>` wire queries dispatch into the `bupha`
//! engine lib through this binary's own `run` verb: each call
//! re-execs `tamias-driver-bupha run <argv>` in a captured child, so
//! `bupha::run_from` writes to the child's stdout and never touches
//! the serve loop's ndjson stream.
//!
//! `ops` answers the ops manifest as JSON — *derived from the
//! engine's own clap tree* ([`bupha::command`]), so a verb can't
//! exist on the CLI and be missing here: declared args become
//! typed params (`arg`/`opt`/`flag`), doc strings ride along
//! unchanged. The CLI's one global (`--config`) rides the
//! manifest's `globals` list once, not per-op.
//!
//! Unbounded daemons can never be request/response ops — they
//! never return, so they ride `surfaces` instead: surface drivers
//! (tamias-driver-cli / -mcp, the pavo-driver-kit `surface` layer,
//! or this binary's feature-gated `cli`/`mcp` verbs) expose them
//! as streaming subcommands — `tamias-driver-bupha cli watch` ≡
//! `bupha watch`. `service logs --follow` streams journalctl —
//! the `follow` param is skipped so the bounded verb stays an op.
//! Hidden plumbing (`gen-cli-docs`) stays reachable through `run`
//! passthrough only.
//!
//! `mutates` is honest: bupha mutates CI state — `deploy`/`destroy`/
//! `service` drive systemd units, forge webhooks, the env file and
//! crates.io releases; `init` scaffolds repos; `import --out`
//! writes a config; `run` executes the watched pipeline (worktrees,
//! runners, status sinks). Read-only ops are `doctor`, `runs`,
//! `log`, `explain`, `deploy check`, `service status|logs`.
//!
//! Secrets are env-var NAMES only (`--token-env`, `secret_env`
//! params, `deploy env`'s migration reads the process env) —
//! values resolve through the canonical chain inside the child and
//! are never argv, never printed by the ops layer.

use clap::{Arg, ArgAction, Command};
use pavo_protocol::wire::{QueryResult, Response};
use serde_json::{json, Map, Value};

/// Unbounded surfaces — verbs that never return (the watcher
/// engine loop, the remote-run agent's HTTP listener, the GHA
/// broker poll loop). They can never be request/response ops;
/// they need inherited stdio.
const SURFACES: &[&str] = &["watch", "agent", "gha-runner"];

/// Params excluded from ops — flags that turn a bounded verb into
/// a stream. `service logs --follow` tails journalctl forever;
/// the op keeps the bounded tail (`-n`), the stream rides the
/// surface. No secret *values* are params — names only, always.
const SKIP_PARAMS: &[&str] = &["follow"];

/// Read-only verbs — everything else writes CI state: systemd
/// units, forge webhooks, `~/.config/bupha/env`, worktrees,
/// crates.io releases, or executes the pipeline. Mutators belong
/// behind a deliberate trigger, not ambient view polling.
const READ_ONLY: &[&str] = &[
    "doctor",
    "runs",
    "log",
    "explain",
    "deploy.check",
    "service.status",
    "service.logs",
];

/// `(name, clap spec)` for every leaf verb. Hidden plumbing and
/// clap's `help` are skipped — they stay reachable through `run`
/// passthrough.
fn verbs() -> Vec<(String, Command)> {
    fn walk(prefix: &str, cmd: &Command, out: &mut Vec<(String, Command)>) {
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            if name == "help" || sub.is_hide_set() {
                continue;
            }
            let dotted = match prefix {
                "" => name.to_string(),
                _ => format!("{prefix}.{name}"),
            };
            if sub.has_subcommands() {
                walk(&dotted, sub, out);
            } else {
                out.push((dotted, sub.clone()));
            }
        }
    }
    let mut out = Vec::new();
    walk("", &bupha::command(), &mut out);
    out
}

fn op_cmd(name: &str) -> Option<Command> {
    verbs()
        .into_iter()
        .find(|(n, _)| n == name && !SURFACES.contains(&n.as_str()))
        .map(|(_, c)| c)
}

/// Args that can't ride the wire: streaming flags (above) and
/// clap's own help/version actions. Globals are handled
/// separately — listed once on the manifest, prepended in
/// [`argv`], never per-op.
fn skipped(a: &Arg) -> bool {
    SKIP_PARAMS.contains(&a.get_id().as_str())
        || matches!(
            a.get_action(),
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        )
}

/// A clap arg → the manifest's typed param kind — `arg` = required
/// positional, `opt` = `--name <v>` *or* an optional positional
/// (the generated surfaces spell it `--name`; `argv` still lands it
/// positionally — `runs.repo`, `run.function`, `init.dir`),
/// `flag` = `--name`.
fn kind(a: &Arg) -> &'static str {
    if a.is_positional() {
        if a.is_required_set() {
            "arg"
        } else {
            "opt"
        }
    } else if matches!(
        a.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    ) {
        "flag"
    } else {
        "opt"
    }
}

fn param_json(a: &Arg) -> Value {
    // Wire name = the flag name (`--no-start` → `no-start`), not
    // the field id (`no_start`) — generated surfaces spell params
    // `--<name>`.
    let id = a.get_id().as_str();
    let name = match a.is_positional() {
        true => id,
        false => a.get_long().unwrap_or(id),
    };
    json!({
        "name": name,
        "kind": kind(a),
        "doc": a.get_help().map(|d| d.to_string()).unwrap_or_default(),
    })
}

/// The `ops` query/`ops` argv verb payload — self-describing so
/// hosts and agents discover the engine surface without argv help.
/// The `--config` global is accepted by every op — listed once on
/// the manifest; the child gets it as an argv flag.
pub fn manifest() -> Value {
    json!({
        "engine": "bupha",
        "invoke": "query `cli.<verb>` with the listed params, or argv `tamias-driver-bupha run <bupha argv>`",
        "globals": bupha::command()
            .get_arguments()
            .filter(|a| a.is_global_set() && !skipped(a))
            .map(param_json)
            .collect::<Vec<_>>(),
        "ops": verbs()
            .iter()
            .filter(|(n, _)| !SURFACES.contains(&n.as_str()))
            .map(|(n, c)| json!({
                "query": format!("cli.{n}"),
                "doc": c.get_about().map(|d| d.to_string()).unwrap_or_default(),
                "params": c.get_arguments()
                    .filter(|a| !skipped(a) && !a.is_global_set())
                    .map(param_json)
                    .collect::<Vec<_>>(),
                "mutates": !READ_ONLY.contains(&n.as_str()),
            }))
            .collect::<Vec<_>>(),
        "surfaces": SURFACES,
    })
}

/// Is `query` an engine op (`cli.<verb>`)? Returns the verb name.
pub fn op_of(query: &str) -> Option<String> {
    query
        .strip_prefix("cli.")
        .filter(|n| op_cmd(n).is_some())
        .map(str::to_string)
}

/// A wire scalar → its argv token (numbers/bools stringify).
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// All values of a param — arrays flatten, scalars wrap.
fn values(v: &Value) -> Vec<String> {
    match v {
        Value::Array(xs) => xs.iter().filter_map(scalar).collect(),
        other => scalar(other).into_iter().collect(),
    }
}

/// Emit `--<long> <v>` / `--<long>` for one arg when `params`
/// carries it — looked up by wire name (long flag), field id, and
/// kebab-case id, so callers may spell params either way. Flags
/// read truthiness; opts/args take every value.
fn push_param(out: &mut Vec<String>, a: &Arg, params: &Map<String, Value>) {
    let id = a.get_id().as_str();
    let long = a.get_long().unwrap_or(id);
    let kebab = id.replace('_', "-");
    let raw = params
        .get(long)
        .or_else(|| params.get(id))
        .or_else(|| params.get(kebab.as_str()));
    let Some(raw) = raw else { return };
    match kind(a) {
        "flag" => {
            if raw.as_bool().unwrap_or(false) {
                out.push(format!("--{long}"));
            }
        }
        _ => {
            for s in values(raw) {
                out.push(format!("--{long}"));
                out.push(s);
            }
        }
    }
}

/// `cli.<verb>` params → bupha argv (no argv[0] — `run` prepends
/// it). Globals go first (clap accepts them either side of the
/// verb), then the verb's named flags, then its positionals in
/// schema order.
fn argv(name: &str, cmd: &Command, params: &Map<String, Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in bupha::command().get_arguments() {
        if a.is_global_set() && !skipped(a) {
            push_param(&mut out, a, params);
        }
    }
    out.extend(name.split('.').map(String::from));
    for a in cmd.get_arguments() {
        if skipped(a) || a.is_positional() || a.is_global_set() {
            continue;
        }
        push_param(&mut out, a, params);
    }
    // `trailing_var_arg`/`last` positionals only parse after a
    // `--` separator — none today, kept generic.
    if cmd.get_positionals().any(|a| a.is_last_set()) {
        out.push("--".into());
    }
    let mut positionals: Vec<String> = Vec::new();
    for a in cmd.get_positionals() {
        if skipped(a) {
            continue;
        }
        let id = a.get_id().as_str();
        let raw = params
            .get(id)
            .or_else(|| params.get(id.replace('_', "-").as_str()));
        if let Some(raw) = raw {
            positionals.extend(values(raw));
        }
    }
    out.extend(positionals);
    out
}

/// The bare-name entry the embedded surfaces use (`OpSpec.name` is
/// `cli.`-stripped); the wire path goes `op_of` → `run_verb`.
#[cfg(any(feature = "cli", feature = "mcp"))]
pub fn run_op(op: &str, params: &Map<String, Value>) -> Response {
    match op_of(&format!("cli.{}", op.strip_prefix("cli.").unwrap_or(op))) {
        Some(name) => run_verb(&name, params),
        None => Response::err(format!("unknown op: {op}")),
    }
}

/// Run one engine op by name: re-exec `run` in a child with
/// captured output, and hand back `{op, argv, code, stdout,
/// stderr}` as the query payload. `ok:false` is reserved for
/// unknown ops and spawn failures — a nonzero `code` is the
/// engine's own exit status, honest data. The child's stdin is
/// null and cwd is inherited — unbounded/host-interactive verbs
/// (`watch`/`agent`/`gha-runner`, `service logs --follow`) are
/// surfaces, never ops, precisely because they need inherited
/// stdio.
pub fn run_verb(name: &str, params: &Map<String, Value>) -> Response {
    let name = name.strip_prefix("cli.").unwrap_or(name);
    let Some(cmd) = op_cmd(name) else {
        return Response::err(format!("unknown op: {name}"));
    };
    let args = argv(name, &cmd, params);
    let out = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg("run")
            .args(&args)
            .output()
    });
    let data = match out {
        Ok(o) => json!({
            "op": name,
            "argv": args,
            "code": o.status.code().unwrap_or(-1),
            "stdout": String::from_utf8_lossy(&o.stdout),
            "stderr": String::from_utf8_lossy(&o.stderr),
        }),
        Err(e) => return Response::err(format!("run {name}: {e}")),
    };
    Response::ok_typed(&QueryResult {
        query: format!("cli.{name}"),
        data,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn op_of_accepts_cli_prefix_only() {
        assert_eq!(op_of("cli.doctor").unwrap(), "doctor");
        assert_eq!(op_of("cli.deploy.release").unwrap(), "deploy.release");
        assert!(op_of("cli.watch").is_none()); // unbounded surface
        assert!(op_of("cli.agent").is_none()); // unbounded surface
        assert!(op_of("cli.gha-runner").is_none()); // unbounded surface
        assert!(op_of("cli.gen-cli-docs").is_none()); // hidden plumbing
        assert!(op_of("doctor").is_none()); // view query, not an op
        assert!(op_of("cli.bogus").is_none());
    }

    #[test]
    fn manifest_lists_every_non_surface_verb_as_cli_query() {
        let m = manifest();
        let ops = m["ops"].as_array().unwrap();
        for (n, _) in verbs()
            .iter()
            .filter(|(n, _)| !SURFACES.contains(&n.as_str()))
        {
            assert!(
                ops.iter().any(|o| o["query"] == json!(format!("cli.{n}"))),
                "{n} missing from ops manifest"
            );
        }
        assert_eq!(m["surfaces"], json!(SURFACES));
        for o in ops {
            for p in o["params"].as_array().unwrap() {
                assert!(["arg", "opt", "flag"].contains(&p["kind"].as_str().unwrap()));
                // streaming params never ride the wire
                assert!(!SKIP_PARAMS.contains(&p["name"].as_str().unwrap()));
                // globals are manifest-level, never per-op params
                assert!(!m["globals"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|g| g["name"] == p["name"]));
            }
        }
        // mutates honesty — read-only set is exactly these; every
        // other bounded verb mutates CI state (units, hooks, env
        // file, releases, worktrees, the pipeline itself).
        for name in READ_ONLY {
            let q = format!("cli.{name}");
            let o = ops.iter().find(|o| o["query"] == json!(q)).unwrap();
            assert_eq!(o["mutates"], json!(false), "{name} must be read-only");
        }
        for name in [
            "init",
            "run",
            "import",
            "deploy.service",
            "deploy.webhook",
            "deploy.env",
            "deploy.release",
            "destroy.worktrees",
            "service.enable",
            "service.restart",
        ] {
            let q = format!("cli.{name}");
            let o = ops.iter().find(|o| o["query"] == json!(q)).unwrap();
            assert_eq!(o["mutates"], json!(true), "{name} must mutate");
        }
        // the streaming flag is gone from service.logs' params
        let logs = ops
            .iter()
            .find(|o| o["query"] == json!("cli.service.logs"))
            .unwrap();
        assert!(!logs["params"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == json!("follow")));
    }

    #[test]
    fn argv_maps_globals_and_nested_verbs() {
        let cmd = op_cmd("doctor").unwrap();
        let mut p = Map::new();
        p.insert("config".into(), json!("/tmp/bupha.yaml"));
        p.insert("json".into(), json!(true));
        assert_eq!(
            argv("doctor", &cmd, &p),
            vec!["--config", "/tmp/bupha.yaml", "doctor", "--json"]
        );

        // nested verb → dotted argv path
        let cmd = op_cmd("service.restart").unwrap();
        assert_eq!(
            argv("service.restart", &cmd, &Map::new()),
            vec!["service", "restart"]
        );
    }

    #[test]
    fn argv_maps_positionals_and_named_params() {
        let cmd = op_cmd("explain").unwrap();
        let mut p = Map::new();
        p.insert("sha".into(), json!("abc1234"));
        assert_eq!(argv("explain", &cmd, &p), vec!["explain", "abc1234"]);

        let cmd = op_cmd("log").unwrap();
        let mut p = Map::new();
        p.insert("n".into(), json!(200));
        // -n is a short-only opt — wire name is the field id `n`.
        assert_eq!(argv("log", &cmd, &p), vec!["log", "--n", "200"]);
    }

    #[test]
    fn argv_flattens_repeatable_dirs() {
        let cmd = op_cmd("import").unwrap();
        let mut p = Map::new();
        p.insert("file".into(), json!("campaign.yaml"));
        p.insert("dir".into(), json!(["o/r=/tmp/r", "o/s=/tmp/s"]));
        p.insert("pull-request".into(), json!(true));
        assert_eq!(
            argv("import", &cmd, &p),
            vec![
                "import",
                "--dir",
                "o/r=/tmp/r",
                "--dir",
                "o/s=/tmp/s",
                "--pull-request",
                "campaign.yaml",
            ]
        );
    }

    #[test]
    fn argv_omits_absent_params() {
        let cmd = op_cmd("runs").unwrap();
        assert_eq!(argv("runs", &cmd, &Map::new()), vec!["runs"]);
    }
}
