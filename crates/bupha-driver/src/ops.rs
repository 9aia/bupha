//! ops — the tamias **content-type** verbs of `tamias-driver-bupha`.
//!
//! tamias discovers `tamias-driver-<type>` executables and runs
//! `<verb> --dir <payload> --name <pkg> [--config <f>]` for the
//! lifecycle three (`install`/`deploy`/`uninstall`) and any provider
//! verb the driver's `capabilities` advertises (`host`, `hooks`,
//! `health`, `status`, `logs`, `config-schema`). This module is that surface —
//! the pavo verbs (`manifest`/`serve`/`capabilities`) stay in
//! `main.rs`/`lib.rs`; `main.rs` dispatches on the subcommand, and
//! tamias verbs always carry `--dir`/`--name` while pavo verbs never
//! do, so the two surfaces can't collide.
//!
//! Config model: the bundle payload ships `fleet.toml` — a complete
//! `bupha_core::Config` doc (every `[[repo]]`, `[runner]`, `[status]`,
//! `[queue]`, …) *except* the two fields the manifest owns:
//! `tunnel.public_url` and `trigger.listen`, which `deploy` injects
//! from `provider.ingress[]` (the same values `hooks register`
//! points GitHub at — one source of truth, no drift). `deploy`
//! writes the merged doc to `~/.config/bupha/bundles/<name>.toml`;
//! the provider units' `tamias-driver-bupha host|worker --config
//! <that path>` consume it (`host` runs the watcher engine
//! in-process via `bupha-watch`, `worker` polls `ci-job` entities
//! on the `[worker] node` — both through the same merged config;
//! ADR-0006: units exec the driver binary, not a cargo-installed
//! `bupha`). The `host`/`hooks`/`provider.ingress[]` path is
//! **standalone/dev mode** (ADR-0007): production webhook/forge
//! ingress is a peregrin campaign listener that normalizes
//! deliveries through bupha's `Trigger` semantics and enqueues
//! `ci-job` via `ci.enqueue`, consumed by the `worker` service —
//! use the listener verbs only where no peregrin runs.
//! Per-bundle files beat
//! merging into `bupha.yaml`: the config
//! loader (`bupha-config-file`) has no include mechanism and a
//! merge-writer would have to track provenance to stay idempotent —
//! a dedicated file is trivially regenerable and `uninstall` is a
//! delete.
//!
//! Secrets are env-var NAMES only (`secret_env`, `token_env`) —
//! values resolve env → cuora → `~/.config/bupha/env`
//! (cuora canonical per T-056), the same chain `bupha` itself uses,
//! and are never printed or written.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

/// The payload file carrying the operator fleet config — a complete
/// `bupha_core::Config` doc minus the manifest-owned
/// `tunnel.public_url`/`trigger.listen`.
const FLEET_FILE: &str = "fleet.toml";

/// tamias manifest names, in tamias-core's resolution order.
const MANIFEST_NAMES: [&str; 3] = ["tamias.yaml", "tamias.yml", "tamias.toml"];

/// The webhook path bupha's listener serves — the only route the
/// `/hook` source (`bupha-watch::webhook`) answers;
/// `provider.ingress[].path` must be this.
const HOOK_PATH: &str = "/hook";

// --- argv -------------------------------------------------------------------

/// Parsed driver flags — the standard `--dir`/`--name`/`--config`
/// triple plus the provider-verb extras. Positional args survive for
/// `hooks <action>`.
#[derive(Debug, Default)]
pub struct Flags {
    /// Payload dir tamias passes (`--dir`).
    pub dir: Option<PathBuf>,
    /// Package name (`--name`).
    pub name: Option<String>,
    /// `--config` override — deploy's output path when given.
    pub config: Option<PathBuf>,
    /// `hooks --dry-run` — print the plan, touch nothing.
    pub dry_run: bool,
    /// `logs --lines N`.
    pub lines: u32,
    /// `hooks --api-base` — forge REST base override.
    pub api_base: Option<String>,
    /// `hooks --token-env` — forge-token secret NAME override.
    pub token_env: Option<String>,
    /// `worker --node` — hypha/peregrin node base URL override
    /// (`[worker] node` / `BUPHA_WORKER_NODE` are the config/env
    /// fallbacks).
    pub node: Option<String>,
    /// `worker --node-token-env` — secret NAME holding the node's
    /// bearer credential (`[worker] node_token_env` fallback).
    pub node_token_env: Option<String>,
    /// `worker --events-url` — full `ci.job.completed` ingest URL
    /// override (`BUPHA_WORKER_EVENTS_URL` / `[worker] events_url`
    /// are the env/config fallbacks; default `{node}/api/events/inject`).
    pub events_url: Option<String>,
    /// Positional args (the `hooks` action).
    pub positional: Vec<String>,
}

/// Parse `--flag value` / `--flag=value` argv. Unknown flags are an
/// error — a typo'd flag must not silently no-op a lifecycle verb.
pub fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut f = Flags {
        lines: 200,
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        let take = |i: &mut usize| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{flag}: missing value"))
        };
        match flag {
            "--dir" => f.dir = Some(PathBuf::from(take(&mut i)?)),
            "--name" => f.name = Some(take(&mut i)?),
            "--config" => f.config = Some(PathBuf::from(take(&mut i)?)),
            "--api-base" => f.api_base = Some(take(&mut i)?),
            "--token-env" => f.token_env = Some(take(&mut i)?),
            "--node" => f.node = Some(take(&mut i)?),
            "--node-token-env" => f.node_token_env = Some(take(&mut i)?),
            "--events-url" => f.events_url = Some(take(&mut i)?),
            "--lines" => {
                f.lines = take(&mut i)?
                    .parse()
                    .map_err(|_| "--lines: expected a number".to_string())?
            }
            "--dry-run" => f.dry_run = true,
            _ if arg.starts_with("--") => return Err(format!("unknown flag {arg}")),
            _ => f.positional.push(arg.clone()),
        }
        i += 1;
    }
    Ok(f)
}

/// `--dir`/`--name` are mandatory on every tamias verb — the dispatch
/// contract guarantees them, but a hand-run driver gets a clear error.
fn required(f: &Flags, verb: &str) -> Result<(PathBuf, String), String> {
    let dir = f
        .dir
        .clone()
        .ok_or_else(|| format!("{verb}: --dir is required"))?;
    let name = f
        .name
        .clone()
        .ok_or_else(|| format!("{verb}: --name is required"))?;
    Ok((dir, name))
}

// --- manifest ---------------------------------------------------------------

/// The slice of `tamias.yaml` this driver reads — the `provider:`
/// block plus the identity fields unit names derive from. Unknown
/// keys are ignored (tamias-core owns full validation at install).
#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    provider: Option<Provider>,
}

#[derive(Debug, Deserialize)]
struct Provider {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    services: Vec<Service>,
    #[serde(default)]
    ingress: Vec<Ingress>,
}

#[derive(Debug, Deserialize)]
struct Service {
    name: String,
}

/// `provider.ingress[]` — the standalone-mode webhook wiring
/// (ADR-0007): public endpoint, listen address, hook path, and the
/// forge hooks `hooks register` manages. Production ingress is a
/// peregrin campaign listener instead; these fields only feed the
/// bupha `host`/`hooks`/`health` path.
#[derive(Debug, Deserialize, Default)]
struct Ingress {
    #[serde(default)]
    public_url: String,
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    hooks: Vec<Hook>,
}

#[derive(Debug, Deserialize)]
struct Hook {
    forge: String,
    repo: String,
    #[serde(default)]
    events: Vec<String>,
}

/// Load `tamias.{yaml,yml,toml}` from the payload dir — the first
/// name present wins, parsed by extension like `bupha-config-file`.
fn load_manifest(dir: &Path) -> Result<Manifest, String> {
    for name in MANIFEST_NAMES {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        return if name.ends_with(".toml") {
            toml::from_str(&text).map_err(|e| format!("{name}: {e}"))
        } else {
            let v: serde_norway::Value =
                serde_norway::from_str(&text).map_err(|e| format!("{name}: {e}"))?;
            serde_norway::from_value(v).map_err(|e| format!("{name}: {e}"))
        };
    }
    Err(format!(
        "{}: no {} found",
        dir.display(),
        MANIFEST_NAMES.join("/")
    ))
}

/// The provider block, or a clear error naming the missing piece.
fn provider<'a>(manifest: &'a Manifest, dir: &Path) -> Result<&'a Provider, String> {
    manifest.provider.as_ref().ok_or_else(|| {
        format!(
            "{}: no provider: block — nothing to deploy/probe",
            dir.display()
        )
    })
}

/// First ingress entry with a non-empty `public_url` — the public
/// endpoint hooks register against and `tunnel.public_url` mirrors.
fn ingress_url(provider: &Provider) -> Option<&Ingress> {
    provider
        .ingress
        .iter()
        .find(|i| !i.public_url.trim().is_empty())
}

/// GitHub forge hooks across every ingress entry, deduped by repo in
/// declaration order — the repos `hooks register` manages and
/// `deploy webhook` would have.
fn github_repos(provider: &Provider) -> Vec<String> {
    let mut repos = Vec::new();
    for hook in provider
        .ingress
        .iter()
        .flat_map(|i| i.hooks.iter())
        .filter(|h| h.forge.eq_ignore_ascii_case("github"))
    {
        if !repos.contains(&hook.repo) {
            repos.push(hook.repo.clone());
        }
    }
    repos
}

// --- config dir / fleet -----------------------------------------------------

/// `~/.config/bupha` — the same resolution `bupha`'s `cfg_dir()` uses:
/// `$XDG_CONFIG_HOME/bupha` → `$HOME/.config/bupha` → `./.config/bupha`.
fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".").join(".config"));
    base.join("bupha")
}

/// `<config>/bundles/<name>.toml` — where `deploy` writes the
/// operator config; `--config` overrides the whole path.
fn bundle_config_path(name: &str, config: Option<&Path>) -> PathBuf {
    config.map_or_else(
        || {
            // Package names are path-safe already; sanitize anyway so a
            // scoped/weird `--name` can't escape the bundles dir.
            let safe: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            config_dir().join("bundles").join(format!("{safe}.toml"))
        },
        Path::to_path_buf,
    )
}

/// `<dir>/fleet.toml` — the payload's operator-config source.
fn fleet_path(dir: &Path) -> PathBuf {
    dir.join(FLEET_FILE)
}

fn read_fleet(dir: &Path) -> Result<String, String> {
    let path = fleet_path(dir);
    std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "{}: {e} — the bundle must ship {FLEET_FILE}",
            path.display()
        )
    })
}

/// The fleet doc as a TOML table — deploy merges the manifest-owned
/// fields into it; hooks/status read it back as a `Config`.
fn fleet_table(dir: &Path) -> Result<toml::Table, String> {
    let text = read_fleet(dir)?;
    match toml::from_str::<toml::Value>(&text) {
        Ok(toml::Value::Table(t)) => Ok(t),
        Ok(_) => Err(format!("{FLEET_FILE}: top level must be a table")),
        Err(e) => Err(format!("{FLEET_FILE}: {e}")),
    }
}

/// The fleet doc parsed as a full `bupha_core::Config` — used by
/// `hooks` for repo/secret names and by `deploy` for validation.
fn fleet_config(dir: &Path) -> Result<bupha_core::Config, String> {
    let text = read_fleet(dir)?;
    let mut cfg: bupha_core::Config =
        toml::from_str(&text).map_err(|e| format!("{FLEET_FILE}: {e}"))?;
    cfg.expand_tilde();
    Ok(cfg)
}

/// Navigate/create nested tables in a `toml::Table` — errors when the
/// fleet already uses the key for a non-table value.
fn table_mut<'a>(root: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table, String> {
    let entry = root
        .entry(key)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    entry
        .as_table_mut()
        .ok_or_else(|| format!("{FLEET_FILE}: '{key}' must be a table"))
}

/// `deploy` — write the operator config
/// `tamias-driver-bupha host|worker --config` consumes: `fleet.toml` with
/// `tunnel.public_url` and
/// `trigger.listen`/`kind` filled from `provider.ingress[]` (the
/// manifest wins those fields — they're the same values the forge
/// hooks point at). Regeneration is idempotent: the file is wholly
/// rewritten from payload + manifest every time.
///
/// # Errors
/// Fails on a missing manifest/provider/fleet, an invalid merged
/// config (`Config::watched` validation), or a write failure.
pub fn deploy(dir: &Path, name: &str, config: Option<&Path>) -> Result<String, String> {
    let manifest = load_manifest(dir)?;
    let provider = provider(&manifest, dir)?;
    let mut fleet = fleet_table(dir)?;

    if let Some(ingress) = ingress_url(provider) {
        let url = ingress.public_url.trim().trim_end_matches('/').to_string();
        if !url.is_empty() {
            {
                let tunnel = table_mut(&mut fleet, "tunnel")?;
                tunnel.insert("public_url".into(), toml::Value::String(url));
            }
            if let Some(listen) = ingress.listen.as_deref().filter(|l| !l.is_empty()) {
                {
                    let trigger = table_mut(&mut fleet, "trigger")?;
                    trigger.insert("listen".into(), toml::Value::String(listen.to_string()));
                    // A declared public ingress means the forge webhook
                    // source — unless the fleet already picked a kind
                    // (e.g. a sturn-only worker config shipped as fleet).
                    trigger
                        .entry("kind")
                        .or_insert_with(|| toml::Value::String("github".into()));
                }
                // cloudflared/direct tunnels forward to the listener —
                // keep local_url consistent with the declared bind.
                let tunnel = table_mut(&mut fleet, "tunnel")?;
                tunnel
                    .entry("local_url")
                    .or_insert_with(|| toml::Value::String(format!("http://{listen}")));
            }
        }
    }

    let text = toml::to_string(&toml::Value::Table(fleet))
        .map_err(|e| format!("{FLEET_FILE}: serialize: {e}"))?;
    // Validate the merged doc exactly as `host --config` will
    // load it — a bad fleet must fail here, not at unit start.
    let mut cfg: bupha_core::Config =
        toml::from_str(&text).map_err(|e| format!("merged config: {e}"))?;
    cfg.expand_tilde();
    cfg.watched().map_err(|e| format!("merged config: {e}"))?;

    let path = bundle_config_path(name, config);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let body = format!(
        "# Generated by tamias-driver-bupha deploy — do not edit.\n\
         # Source: {} fleet.toml + tamias.yaml provider.ingress.\n\
         # Regenerate: tamias deploy {name} (or `tamias driver exec {name} deploy`).\n\
         {text}",
        dir.display()
    );
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!(
        "wrote operator config {} ({} repo entries)",
        path.display(),
        cfg.repos.len()
    ))
}

/// `uninstall` — remove the config `deploy` wrote. `--dir` is
/// accepted for dispatch parity and ignored; `--config`/`--name`
/// resolve the same path deploy used.
pub fn uninstall(name: &str, config: Option<&Path>) -> String {
    let path = bundle_config_path(name, config);
    if path.is_file() {
        let _ = std::fs::remove_file(&path);
    }
    format!("removed operator config {}", path.display())
}

/// `install` — no-op. Like the peregrin driver, the operator config
/// is `deploy`'s job: install only sees the evictable fetch-cache
/// payload dir; the stable `~/.config/bupha/` write belongs to
/// deploy, which `tamias deploy` runs against the store entry.
pub fn install() -> &'static str {
    "install: operator config is generated by `tamias deploy` — nothing to do"
}

// --- secrets -----------------------------------------------------------------

/// `~/.config/bupha/env` — the dotenv leg of the secret chain (same
/// file the provider unit's `env_file` loads).
fn env_file() -> PathBuf {
    config_dir().join("env")
}

/// Secret resolution chain, mirroring `bupha`'s own `secret()`
/// (T-056 — cuora canonical; keyring leg dropped post-T-077):
/// env var → cuora broker (`cuora://bupha/<NAME>` via the `cuora`
/// CLI) → `~/.config/bupha/env` (the dev-mode / no-cuora fallback,
/// also the provider unit's `EnvironmentFile`). Values are used
/// in-flight only — never printed, never written to configs.
fn secret(name: &str) -> Option<String> {
    use hypha_secret::Secrets;
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| bupha_secret_cuora::CuoraSecrets::default().get(name))
        .or_else(|| hypha_dotenv::DotenvSecrets { path: env_file() }.get(name))
}

fn missing_secret(name: &str) -> String {
    format!("{name} required (env, cuora or ~/.config/bupha/env)")
}

// --- hooks -------------------------------------------------------------------

/// One concrete hook target: a github-hook repo + the public URL
/// bupha's `/hook` route hangs off.
struct HookTarget {
    repo: String,
    /// `provider.ingress[].public_url` — `register_webhook` appends
    /// `/hook` itself.
    public_url: String,
    /// Declared event names (bupha triggers on `push` only).
    events: Vec<String>,
    /// The repo's webhook-secret env NAME (fleet `secret_env`, else
    /// the trigger global).
    secret_env: String,
    /// The forge REST base.
    api_base: String,
}

/// Resolve hook targets: manifest `provider.ingress[].hooks[]`
/// (github only) for the repo/URL set, `fleet.toml` for each repo's
/// `secret_env` override and the global defaults. Non-github forges
/// stay declared data — bupha has no forge impl for them.
fn hook_targets(dir: &Path, api_base: Option<&str>) -> Result<Vec<HookTarget>, String> {
    let manifest = load_manifest(dir)?;
    let provider = provider(&manifest, dir)?;
    let url = ingress_url(provider)
        .map(|i| i.public_url.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            format!(
                "{}: provider.ingress declares no public_url — nothing to hook",
                dir.display()
            )
        })?;
    let repos = github_repos(provider);
    if repos.is_empty() {
        return Err(format!(
            "{}: provider.ingress declares no github hooks — nothing to manage",
            dir.display()
        ));
    }
    // Fleet supplies per-repo secret_env + the global fallbacks.
    // Missing fleet → every repo verifies with the global secret,
    // same as a config without overrides.
    let fleet = fleet_config(dir).ok();
    let global_secret = fleet
        .as_ref()
        .map(|c| c.trigger.secret_env.clone())
        .unwrap_or_else(|| "BUPHA_WEBHOOK_SECRET".into());
    let default_api = fleet
        .as_ref()
        .map(|c| c.trigger.api_base.clone())
        .unwrap_or_else(|| "https://api.github.com".into());
    let base = api_base.unwrap_or(&default_api).to_string();
    let mut targets = Vec::new();
    for ingress in &provider.ingress {
        for hook in ingress
            .hooks
            .iter()
            .filter(|h| h.forge.eq_ignore_ascii_case("github"))
        {
            let secret_env = fleet
                .as_ref()
                .and_then(|c| c.repos.iter().find(|r| r.name == hook.repo))
                .and_then(|r| r.secret_env.clone())
                .unwrap_or_else(|| global_secret.clone());
            let events = if hook.events.is_empty() {
                vec!["push".to_string()]
            } else {
                hook.events.clone()
            };
            let t = HookTarget {
                repo: hook.repo.clone(),
                public_url: url.clone(),
                events,
                secret_env,
                api_base: base.clone(),
            };
            if !targets
                .iter()
                .any(|o: &HookTarget| o.repo == t.repo && o.public_url == t.public_url)
            {
                targets.push(t);
            }
        }
    }
    Ok(targets)
}

/// `hooks register|uninstall` — upsert/remove the GitHub webhooks
/// the manifest's `provider.ingress[].hooks[]` declare, via
/// `bupha_github`'s REST path (exact-URL match, idempotent).
///
/// **Standalone mode** (ADR-0007): this registers hooks at the
/// bupha webhook listener — the fallback ingress for deployments
/// with no peregrin. Production ingress is a peregrin campaign
/// listener enqueuing `ci-job` via `ci.enqueue`; don't point
/// production webhooks here.
///
/// Register needs the forge token (`--token-env`/`[runner]
/// token_env`, default `BUPHA_TOKEN`) and each repo's webhook
/// secret by NAME; unregister needs only the token. `--dry-run`
/// prints the plan and resolves nothing.
///
/// # Errors
/// Fails on an unreadable manifest/fleet, missing secrets, or forge
/// API errors. Returns the report text (driver-verb output).
pub fn hooks(f: &Flags, register: bool) -> Result<String, String> {
    let verb = if register { "register" } else { "unregister" };
    let (dir, _name) = required(f, "hooks")?;
    let targets = hook_targets(&dir, f.api_base.as_deref())?;
    if f.dry_run {
        return Ok(targets
            .iter()
            .map(|t| {
                format!(
                    "{}: would {} {}/hook events=[{}] secret={} (name — value never shown)",
                    t.repo,
                    verb,
                    t.public_url,
                    t.events.join(","),
                    t.secret_env
                )
            })
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let fleet = fleet_config(&dir).ok();
    let token_name = f
        .token_env
        .clone()
        .or_else(|| fleet.as_ref().map(|c| c.runner.token_env.clone()))
        .unwrap_or_else(|| "BUPHA_TOKEN".into());
    let token = secret(&token_name).ok_or_else(|| missing_secret(&token_name))?;
    let mut lines = Vec::with_capacity(targets.len());
    for t in &targets {
        let msg = if register {
            // bupha triggers on push deliveries only — a manifest
            // declaring more gets them noted, not registered.
            if t.events.iter().any(|e| e != "push") {
                eprintln!(
                    "warning: {} declares events [{}] — bupha registers [push] only",
                    t.repo,
                    t.events.join(",")
                );
            }
            let wh = secret(&t.secret_env).ok_or_else(|| missing_secret(&t.secret_env))?;
            bupha_github::register_webhook(&token, &t.repo, &t.public_url, &wh, &t.api_base)
                .map_err(|e| format!("webhook {}: {e}", t.repo))?
        } else {
            bupha_github::unregister_webhook(&token, &t.repo, &t.public_url, &t.api_base)
                .map_err(|e| format!("webhook {}: {e}", t.repo))?
        };
        lines.push(msg);
    }
    Ok(lines.join("\n"))
}

// --- probes ------------------------------------------------------------------

/// `systemctl --user <args>` — stdout trimmed, else stderr trimmed,
/// else `None` when systemd can't answer (no user bus, missing
/// binary). Exit status ignored: `is-active`/`is-enabled` exit
/// non-zero for inactive/disabled/not-found states.
fn systemctl(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return (!err.is_empty()).then_some(err);
    }
    Some(text)
}

/// The provider's unit base names — `<provider-or-package>-<service>`
/// `.service`, the same derivation `tamias deploy` renders
/// (`tamias-provider::unit_name`).
fn provider_units(manifest: &Manifest, fallback_name: &str) -> Vec<(String, String)> {
    let Some(provider) = &manifest.provider else {
        return Vec::new();
    };
    let base = provider
        .name
        .clone()
        .unwrap_or_else(|| match &manifest.scope {
            Some(scope) => format!("{scope}-{}", manifest.name),
            None if manifest.name.is_empty() => fallback_name.to_string(),
            None => manifest.name.clone(),
        });
    provider
        .services
        .iter()
        .map(|s| (s.name.clone(), format!("{base}-{}.service", s.name)))
        .collect()
}

/// `health` — probe each ingress `listen`+`path` with the webhook
/// `ping` (`POST <listen><path>` + `X-GitHub-Event: ping` → `200
/// pong`), falling back to `systemctl --user is-active` per provider
/// unit when no ingress declares both. Prints a JSON status object.
///
/// # Errors
/// Fails when the manifest can't be read or nothing is probeable.
pub fn health(dir: &Path, name: &str) -> Result<String, String> {
    let manifest = load_manifest(dir)?;
    let provider = provider(&manifest, dir)?;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(5)))
        .build()
        .new_agent();
    let mut probes = Vec::new();
    for ingress in &provider.ingress {
        let (Some(listen), path) = (&ingress.listen, ingress.path.as_deref()) else {
            continue;
        };
        // The webhook source answers only /hook — an undeclared path
        // probes the real route, a declared non-/hook path still
        // gets probed as declared (and reports the 404 honestly).
        let path = path.unwrap_or(HOOK_PATH);
        let url = format!("http://{listen}{path}");
        let result = agent
            .post(&url)
            .header("x-github-event", "ping")
            .header("User-Agent", "bupha")
            .send_empty()
            .map_or_else(
                |e| (false, e.to_string()),
                |mut resp| {
                    let status = resp.status().as_u16();
                    let body = resp
                        .body_mut()
                        .read_to_string()
                        .unwrap_or_else(|_| String::new());
                    (status == 200, format!("{status} {}", body.trim()))
                },
            );
        probes.push(json!({
            "kind": "hook-ping",
            "url": url,
            "ok": result.0,
            "detail": result.1,
        }));
    }
    if probes.is_empty() {
        for (service, unit) in provider_units(&manifest, name) {
            let state = systemctl(&["is-active", &unit]).unwrap_or_else(|| "unknown".into());
            probes.push(json!({
                "kind": "unit",
                "service": service,
                "unit": unit,
                "ok": state == "active",
                "detail": state,
            }));
        }
    }
    if probes.is_empty() {
        return Err(format!(
            "{name}: provider declares no ingress listeners or services — nothing to probe"
        ));
    }
    let ok = probes.iter().all(|p| p["ok"].as_bool() == Some(true));
    serde_json::to_string(&json!({"name": name, "ok": ok, "probes": probes}))
        .map_err(|e| e.to_string())
}

/// `status` — per-unit `systemctl --user is-active`/`is-enabled` as a
/// JSON object; units systemd can't answer report `"unknown"`.
///
/// # Errors
/// Fails when the manifest can't be read or declares no services.
pub fn status(dir: &Path, name: &str) -> Result<String, String> {
    let manifest = load_manifest(dir)?;
    provider(&manifest, dir)?;
    let units = provider_units(&manifest, name);
    if units.is_empty() {
        return Err(format!(
            "{name}: provider declares no services — nothing to report"
        ));
    }
    let rows: Vec<Value> = units
        .iter()
        .map(|(service, unit)| {
            let active = systemctl(&["is-active", unit]).unwrap_or_else(|| "unknown".into());
            let enabled = systemctl(&["is-enabled", unit]).unwrap_or_else(|| "unknown".into());
            json!({
                "service": service,
                "unit": unit,
                "active": active,
                "enabled": enabled,
                "ok": active == "active",
            })
        })
        .collect();
    let ok = rows.iter().all(|r| r["ok"].as_bool() == Some(true));
    serde_json::to_string(&json!({"name": name, "ok": ok, "units": rows}))
        .map_err(|e| e.to_string())
}

/// `logs` — `journalctl --user -u <unit>` over every provider unit,
/// newest `lines` entries, no follow (dispatch is bounded — a `-f`
/// would just be killed). Returns the journal text.
///
/// # Errors
/// Fails when the manifest can't be read or declares no services.
pub fn logs(dir: &Path, name: &str, lines: u32) -> Result<String, String> {
    use std::fmt::Write as _;
    let manifest = load_manifest(dir)?;
    provider(&manifest, dir)?;
    let units = provider_units(&manifest, name);
    if units.is_empty() {
        return Err(format!(
            "{name}: provider declares no services — nothing to journal"
        ));
    }
    let n = lines.max(1).to_string();
    let mut out = String::new();
    for (_, unit) in &units {
        let _ = writeln!(out, "── {unit} ──");
        let journal = std::process::Command::new("journalctl")
            .args(["--user", "-u", unit, "-n", &n, "--no-pager"])
            .output()
            .map_err(|e| format!("journalctl {unit}: {e}"))?;
        out.push_str(&String::from_utf8_lossy(&journal.stdout));
        if !journal.status.success() {
            out.push_str(&String::from_utf8_lossy(&journal.stderr));
        }
    }
    Ok(out)
}

// --- config-schema ------------------------------------------------------------

/// `config-schema` — the JSON Schema (draft 2020-12) of the operator
/// config `deploy` writes: a `bupha_core::Config` doc (the fleet
/// TOML/YAML — YAML is a JSON superset for these shapes) with
/// `tunnel.public_url`/`trigger.listen` manifest-owned. Deliberately
/// permissive like the sibling drivers': operators may hand-edit
/// keys the generator doesn't emit, so only the sections a working
/// watcher needs are `required`.
///
/// # Errors
/// Fails only if the schema can't serialize (never in practice).
pub fn config_schema() -> Result<String, String> {
    let repo = json!({
        "type": "object",
        "required": ["name", "dir"],
        "properties": {
            "name": {"type": "string", "description": "owner/repo (or bare name for sturn-only repos) — also the status target."},
            "dir": {"type": "string", "description": "Clone dir; leading ~/ expands."},
            "branch": {"type": "string", "default": "refs/heads/main"},
            "status_repo": {"type": "string"},
            "runner": {"type": "object", "description": "Per-repo runner override — a full [runner] table."},
            "queue": {"type": "string", "enum": ["latest", "all"], "default": "latest"},
            "secret_env": {"type": "string", "description": "Webhook-secret env NAME for this repo (resolved env → cuora → ~/.config/bupha/env (cuora canonical, T-056)). Never a value."},
            "runs_on": {"type": "array", "items": {"type": "string"}, "description": "Fleet labels a claiming worker must carry."},
            "retry": {"type": "integer", "minimum": 0, "maximum": 3},
            "sturn": {"type": "boolean", "description": "Opt into the [trigger.sturn] WAL poller."},
            "jobs": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": {"type": "string"},
                        "branch": {"type": "string"},
                        "runner": {"type": "object"},
                        "queue": {"type": "string", "enum": ["latest", "all"]},
                        "context": {"type": "string"},
                        "runs_on": {"type": "array", "items": {"type": "string"}},
                        "retry": {"type": "integer"}
                    }
                }
            }
        }
    });
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "bupha operator config",
        "description": "The --config file `tamias-driver-bupha host` consumes — written by `tamias deploy` via tamias-driver-bupha from the bundle's fleet.toml + tamias.yaml provider.ingress. TOML/YAML mapping; schema is draft 2020-12.",
        "type": "object",
        "required": ["repo", "runner", "status"],
        "properties": {
            "repo": {
                "type": "array",
                "items": repo,
                "description": "Watched repos ([[repo]]; `repos` alias accepted)."
            },
            "runner": {
                "type": "object",
                "required": ["kind"],
                "properties": {
                    "kind": {"type": "string", "description": "dagger | shell | ssh | container | http | …"},
                    "token_env": {"type": "string", "description": "Forge-token env NAME (default BUPHA_TOKEN)."},
                    "cancel_running": {"type": "boolean"}
                }
            },
            "status": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": ["github", "log", "none"]},
                    "repo": {"type": "string"},
                    "context": {"type": "string", "default": "ci/bupha"}
                }
            },
            "trigger": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "description": "github (default when ingress declares a public_url)"},
                    "api_base": {"type": "string", "default": "https://api.github.com"},
                    "listen": {"type": "string", "description": "Manifest-owned: provider.ingress[].listen."},
                    "secret_env": {"type": "string", "default": "BUPHA_WEBHOOK_SECRET", "description": "Global webhook-secret env NAME; [[repo]] secret_env overrides per repo."},
                    "sturn": {"type": "object", "description": "Optional [trigger.sturn] WAL poller source."}
                }
            },
            "tunnel": {
                "type": "object",
                "properties": {
                    "provider": {"type": "string", "enum": ["none", "cloudflared", "direct"]},
                    "name": {"type": "string"},
                    "local_url": {"type": "string", "default": "http://127.0.0.1:29173"},
                    "public_url": {"type": "string", "description": "Manifest-owned: provider.ingress[].public_url."}
                }
            },
            "queue": {"type": "object", "properties": {"kind": {"type": "string"}}},
            "worktree": {"type": "object", "properties": {"base": {"type": "string", "default": "/tmp/bupha"}}},
            "cache": {"type": "object"},
            "access": {"type": "object"},
            "audit": {"type": "object"},
            "telemetry": {"type": "object"},
            "attest": {"type": "object"},
            "worker": {
                "type": "object",
                "properties": {
                    "labels": {"type": "array", "items": {"type": "string"}},
                    "node": {"type": "string", "description": "Hypha/peregrin node base URL for the `worker` verb's ci-job poll (P-002). Also settable via --node / BUPHA_WORKER_NODE."},
                    "node_token_env": {"type": "string", "description": "Secret NAME (env → cuora → env file) holding the node bearer credential; BUPHA_NODE_TOKEN is a direct-token fallback."},
                    "events_url": {"type": "string", "description": "Full ci.job.completed ingest URL override (e.g. campaign serve's /api/events/inject); also settable via --events-url / BUPHA_WORKER_EVENTS_URL. Default {node}/api/events/inject."},
                    "fork_token_env": {"type": "string", "description": "Secret NAME (env → cuora → env file) holding a git credential for private fork cloneUrl fetches on fork-PR ci-jobs; resolved by a generated credential-helper at fetch time — NAME only, never the value."}
                }
            },
            "agent": {"type": "object"}
        }
    });
    serde_json::to_string_pretty(&schema).map_err(|e| e.to_string())
}

/// `host` — engine-host mode: run the watcher pipeline
/// **in-process** so deployed provider units exec this driver
/// binary instead of a cargo-installed `bupha` (ADR-0006: no
/// user-facing bupha CLI on the host path; bupha T-052).
///
/// Loads the operator config `deploy` wrote — `--config <path>`,
/// else `~/.config/bupha/bundles/<name>.toml` (`--name` required
/// then) — and blocks in `bupha_watch::watch`: trigger listener +
/// queues + runner + status sinks, identical to `bupha watch`.
/// `--dir` is accepted (tamias's dispatch contract) but unused —
/// the payload's `fleet.toml` only matters at `deploy` time.
///
/// # Errors
/// Fails on a missing `--config`/`--name` or an unloadable config;
/// once running, config errors inside `watch` exit the process
/// like the CLI's did.
pub fn host(f: &Flags) -> Result<String, String> {
    let path = match f.config.clone() {
        Some(p) => p,
        None => bundle_config_path(
            f.name
                .as_deref()
                .ok_or_else(|| "host: --config or --name is required".to_string())?,
            None,
        ),
    };
    let cfg = bupha_config_file::FileSource {
        path: Some(path.to_string_lossy().to_string()),
    }
    .load()
    .map_err(|e| format!("host: {}: {e}", path.display()))?;
    // Blocks until the sources stop (e2e shutdown hook, signal) —
    // the unit's Restart= policy owns the lifecycle from there.
    bupha_watch::watch(std::sync::Arc::new(cfg));
    Ok(String::new())
}

/// `worker` — the `ci-job` consumption loop (P-002 §4.1): poll
/// `kind="ci-job"` entities on a hypha/peregrin node, `ci.claim`
/// the pending ones this box's `[worker] labels` cover, run them
/// through the same `bupha-watch` pipeline `host` uses (worktree →
/// runner → status sink), `ci.renew` on the run ticker,
/// `ci.complete` with the result. This is the **second service** a
/// fleet deployment runs alongside `host` — the watcher ingests
/// pushes (standalone mode) while workers execute jobs the
/// entity plane hands out; in peregrin-hosted mode the plane lives
/// on the node and `host` isn't needed at all.
///
/// Node endpoint resolution: `--node` → `BUPHA_WORKER_NODE` →
/// `[worker] node` in the operator config. Credential: `--node-token-env`
/// / `[worker] node_token_env` names a secret resolved through the
/// usual chain (env → cuora → `~/.config/bupha/env`);
/// `BUPHA_NODE_TOKEN` is a direct-token escape hatch; neither set =
/// unauthenticated (dev nodes).
///
/// # Errors
/// Fails on a missing `--config`/`--name`, an unloadable config, a
/// missing node endpoint, or an unresolvable token NAME; once
/// running, poll errors back off and retry.
pub fn worker(f: &Flags) -> Result<String, String> {
    let path = match f.config.clone() {
        Some(p) => p,
        None => bundle_config_path(
            f.name
                .as_deref()
                .ok_or_else(|| "worker: --config or --name is required".to_string())?,
            None,
        ),
    };
    use bupha_watch::entity_worker::JobExec as _;
    let cfg = bupha_config_file::FileSource {
        path: Some(path.to_string_lossy().to_string()),
    }
    .load()
    .map_err(|e| format!("worker: {}: {e}", path.display()))?;

    let node = f
        .node
        .clone()
        .or_else(|| {
            std::env::var("BUPHA_WORKER_NODE")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| cfg.worker.node.clone())
        .ok_or_else(|| {
            "worker: no node endpoint — set [worker] node, --node, or BUPHA_WORKER_NODE".to_string()
        })?;
    let token = match f
        .node_token_env
        .clone()
        .or_else(|| cfg.worker.node_token_env.clone())
    {
        Some(name) => Some(
            bupha_watch::secret(&name)
                .ok_or_else(|| format!("worker: {name} required (env, cuora or env file)"))?,
        ),
        None => std::env::var("BUPHA_NODE_TOKEN")
            .ok()
            .filter(|s| !s.is_empty()),
    };

    let events_url = f
        .events_url
        .clone()
        .or_else(|| {
            std::env::var("BUPHA_WORKER_EVENTS_URL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| cfg.worker.events_url.clone());
    let api = bupha_watch::entity_worker::EntityJobApi::http(&node, token)
        .map(|api| api.with_events_url(events_url))
        .map_err(|e| format!("worker: {e}"))?;
    let filter = bupha_watch::entity_worker::ClaimFilter::from_config(&cfg)
        .map_err(|e| format!("worker: config: {e}"))?;
    let exec = bupha_watch::entity_worker::WatchExec::new(std::sync::Arc::new(cfg));
    // Same e2e shutdown hook as `host` — the loop
    // checks it between jobs (a run in flight finishes first).
    let shutdown = || {
        std::env::var("BUPHA_WATCH_SHUTDOWN_FILE")
            .ok()
            .is_some_and(|f| Path::new(&f).exists())
    };
    println!(
        "ci-job worker polling entities on {node} ({})",
        exec.worker()
    );
    bupha_watch::entity_worker::run(&filter, &api, &exec, &shutdown);
    Ok(String::new())
}

// --- dispatch -----------------------------------------------------------------

/// Run one tamias verb. `verb` is the subcommand `main.rs` already
/// matched; `args` is the remaining argv (`--dir … --name …` plus
/// verb flags). Prints the verb's report to stdout; errors go to
/// stderr with a non-zero exit — the tamias dispatch contract.
pub fn run(verb: &str, args: &[String]) -> i32 {
    let result = match verb {
        "install" => parse_flags(args).and_then(|f| {
            required(&f, "install")?;
            Ok(install().to_string())
        }),
        "deploy" => parse_flags(args).and_then(|f| {
            let (dir, name) = required(&f, "deploy")?;
            deploy(&dir, &name, f.config.as_deref())
        }),
        "uninstall" => parse_flags(args).and_then(|f| {
            let (_dir, name) = required(&f, "uninstall")?;
            Ok(uninstall(&name, f.config.as_deref()))
        }),
        "hooks" => parse_flags(args).and_then(|f| {
            let action = f
                .positional
                .first()
                .map(String::as_str)
                .ok_or_else(|| "hooks: expected `register` or `unregister`".to_string())?;
            match action {
                "register" => hooks(&f, true),
                "unregister" | "uninstall" => hooks(&f, false),
                other => Err(format!("hooks: unknown action '{other}'")),
            }
        }),
        "health" => parse_flags(args).and_then(|f| {
            let (dir, name) = required(&f, "health")?;
            health(&dir, &name)
        }),
        "status" => parse_flags(args).and_then(|f| {
            let (dir, name) = required(&f, "status")?;
            status(&dir, &name)
        }),
        "logs" => parse_flags(args).and_then(|f| {
            let (dir, name) = required(&f, "logs")?;
            logs(&dir, &name, f.lines)
        }),
        "host" => parse_flags(args).and_then(|f| host(&f)),
        "worker" => parse_flags(args).and_then(|f| worker(&f)),
        "config-schema" => {
            // Instance-agnostic probe — flags accepted, ignored.
            match config_schema() {
                Ok(s) => {
                    println!("{s}");
                    return 0;
                }
                Err(e) => Err(e),
            }
        }
        other => Err(format!("unknown verb '{other}'")),
    };
    match result {
        Ok(output) => {
            if !output.is_empty() {
                println!("{output}");
            }
            0
        }
        Err(e) => {
            eprintln!("tamias-driver-bupha: {e}");
            1
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::data::tests_util::tmp;

    const MIN_FLEET: &str = r#"
[[repo]]
name = "o/r"
dir = "/tmp/o-r"
secret_env = "BUPHA_WH_O_R"

[runner]
kind = "shell"

[status]
kind = "none"
"#;

    const MANIFEST: &str = r#"
name: ci-gate
version: "0.1.0"
type: bupha
provider:
  name: bupha
  services:
    - name: watch
      exec: "tamias-driver-bupha host --config ~/.config/bupha/bundles/ci-gate.toml"
  ingress:
    - public_url: https://ci.example.com
      listen: 127.0.0.1:29173
      path: /hook
      hooks:
        - forge: github
          repo: o/r
          events: [push]
        - forge: gitlab
          repo: ignored/g
          events: [push]
"#;

    /// A payload dir with `tamias.yaml` + `fleet.toml`.
    fn bundle(tag: &str) -> PathBuf {
        let dir = tmp(tag);
        std::fs::write(dir.join("tamias.yaml"), MANIFEST).unwrap();
        std::fs::write(dir.join(FLEET_FILE), MIN_FLEET).unwrap();
        dir
    }

    fn flags(args: &[&str]) -> Flags {
        parse_flags(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn parse_flags_pairs_and_inline() {
        let f = flags(&[
            "--dir",
            "/p",
            "--name=x",
            "--config",
            "/c",
            "--dry-run",
            "--lines=50",
            "register",
        ]);
        assert_eq!(f.dir, Some(PathBuf::from("/p")));
        assert_eq!(f.name.as_deref(), Some("x"));
        assert_eq!(f.config, Some(PathBuf::from("/c")));
        assert!(f.dry_run);
        assert_eq!(f.lines, 50);
        assert_eq!(f.positional, vec!["register".to_string()]);
    }

    #[test]
    fn parse_flags_rejects_unknown_and_missing_value() {
        assert!(parse_flags(&["--bogus".into()]).is_err());
        assert!(parse_flags(&["--dir".into()]).is_err());
    }

    #[test]
    fn deploy_writes_merged_operator_config() {
        let dir = bundle("deploy");
        let out = dir.join("out.toml");
        let line = deploy(&dir, "ci-gate", Some(&out)).unwrap();
        assert!(line.contains("1 repo entries"), "{line}");
        let text = std::fs::read_to_string(&out).unwrap();
        // manifest-owned fields injected
        assert!(text.contains("https://ci.example.com"));
        assert!(text.contains("127.0.0.1:29173"));
        // fleet content preserved
        assert!(text.contains("o/r"));
        assert!(text.contains("BUPHA_WH_O_R"));
        // parses back through the same loader `host --config` uses
        let cfg = bupha_config_file::FileSource {
            path: Some(out.to_string_lossy().to_string()),
        }
        .load()
        .unwrap();
        assert_eq!(cfg.repos.len(), 1);
        assert_eq!(cfg.tunnel.public_url, "https://ci.example.com");
        assert_eq!(cfg.trigger.listen, "127.0.0.1:29173");
        assert_eq!(cfg.trigger.kind, "github");
        assert_eq!(cfg.tunnel.local_url, "http://127.0.0.1:29173");
    }

    #[test]
    fn deploy_is_idempotent() {
        let dir = bundle("deploy-idem");
        let out = dir.join("out.toml");
        deploy(&dir, "ci-gate", Some(&out)).unwrap();
        let first = std::fs::read_to_string(&out).unwrap();
        deploy(&dir, "ci-gate", Some(&out)).unwrap();
        assert_eq!(first, std::fs::read_to_string(&out).unwrap());
    }

    #[test]
    fn deploy_requires_fleet_and_provider() {
        let dir = tmp("deploy-nofleet");
        std::fs::write(dir.join("tamias.yaml"), MANIFEST).unwrap();
        let out = dir.join("out.toml");
        let e = deploy(&dir, "x", Some(&out)).unwrap_err();
        assert!(e.contains(FLEET_FILE), "{e}");
        assert!(!out.exists());

        let dir2 = tmp("deploy-noprovider");
        std::fs::write(dir2.join("tamias.yaml"), "name: x\n").unwrap();
        std::fs::write(dir2.join(FLEET_FILE), MIN_FLEET).unwrap();
        let e = deploy(&dir2, "x", Some(&dir2.join("o.toml"))).unwrap_err();
        assert!(e.contains("provider"), "{e}");
    }

    #[test]
    fn deploy_rejects_invalid_fleet() {
        let dir = tmp("deploy-badfleet");
        std::fs::write(dir.join("tamias.yaml"), MANIFEST).unwrap();
        std::fs::write(dir.join(FLEET_FILE), "repo = 42\n").unwrap();
        let e = deploy(&dir, "x", Some(&dir.join("o.toml"))).unwrap_err();
        assert!(!e.is_empty());
    }

    #[test]
    fn uninstall_removes_deployed_config() {
        let dir = bundle("uninstall");
        let out = dir.join("out.toml");
        deploy(&dir, "ci-gate", Some(&out)).unwrap();
        assert!(out.is_file());
        let line = uninstall("ci-gate", Some(&out));
        assert!(line.contains("removed"));
        assert!(!out.exists());
        // idempotent — removing twice is fine
        uninstall("ci-gate", Some(&out));
    }

    #[test]
    fn install_is_a_noop() {
        assert!(install().contains("nothing to do"));
    }

    #[test]
    fn hooks_dry_run_prints_plan_without_secrets() {
        let dir = bundle("hooks-dry");
        let f = flags(&[
            "register",
            "--dir",
            dir.to_str().unwrap(),
            "--name",
            "ci-gate",
            "--dry-run",
        ]);
        let out = hooks(&f, true).unwrap();
        assert!(out.contains("would register"), "{out}");
        assert!(out.contains("o/r"));
        assert!(out.contains("BUPHA_WH_O_R"));
        assert!(out.contains("/hook"));
        // gitlab hooks are not targets
        assert!(!out.contains("ignored/g"));
    }

    #[test]
    fn hooks_requires_action_and_provider() {
        let dir = bundle("hooks-args");
        // No positional action — `run` rejects before touching state.
        let args: Vec<String> = ["--dir", dir.to_str().unwrap(), "--name", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(run("hooks", &args), 1);
        // Action but no provider: block — hook_targets errors.
        let dir2 = tmp("hooks-noprovider");
        std::fs::write(dir2.join("tamias.yaml"), "name: x\n").unwrap();
        let f2 = flags(&[
            "register",
            "--dir",
            dir2.to_str().unwrap(),
            "--name",
            "x",
            "--dry-run",
        ]);
        assert!(hooks(&f2, true).unwrap_err().contains("provider"));
    }

    /// Minimal github-trigger config on an ephemeral port: the
    /// webhook secret resolves from the env (NAME only), and the
    /// e2e shutdown hook (`BUPHA_WATCH_SHUTDOWN_FILE`) unwinds the
    /// accept loop so `host` returns.
    #[test]
    fn host_runs_engine_until_shutdown_hook() {
        let dir = tmp("host-run");
        let repo_dir = dir.join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let cfg_path = dir.join("bupha.toml");
        std::fs::write(
            &cfg_path,
            format!(
                r#"
[[repo]]
name = "o/r"
dir = "{}"

[runner]
kind = "shell"

[status]
kind = "none"

[trigger]
kind = "github"
listen = "127.0.0.1:0"
secret_env = "BUPHA_WEBHOOK_SECRET"

[worktree]
base = "{}"
"#,
                repo_dir.display(),
                dir.join("wt").display()
            ),
        )
        .unwrap();
        let stop = dir.join("stop");
        std::fs::write(&stop, "").unwrap();
        std::env::set_var("BUPHA_WATCH_SHUTDOWN_FILE", &stop);
        std::env::set_var("BUPHA_WEBHOOK_SECRET", "test-secret");
        let args: Vec<String> = [
            "--config".to_string(),
            cfg_path.to_string_lossy().to_string(),
        ]
        .to_vec();
        assert_eq!(run("host", &args), 0);
        std::env::remove_var("BUPHA_WATCH_SHUTDOWN_FILE");
        std::env::remove_var("BUPHA_WEBHOOK_SECRET");
    }

    #[test]
    fn host_requires_config_or_name() {
        assert_eq!(run("host", &[]), 1);
        let missing = tmp("host-missing").join("nope.toml");
        let args: Vec<String> = [
            "--config".to_string(),
            missing.to_string_lossy().to_string(),
        ]
        .to_vec();
        assert_eq!(run("host", &args), 1);
    }

    #[test]
    fn worker_requires_config_and_node() {
        // No --config/--name → usage failure. (Env cleared so an
        // externally-set BUPHA_WORKER_NODE can't push the test into
        // the blocking poll loop.)
        std::env::remove_var("BUPHA_WORKER_NODE");
        assert_eq!(run("worker", &[]), 1);
        // A loadable config without a node endpoint fails fast —
        // the polling loop itself is covered by bupha-watch's
        // entity_worker tests (it would block here).
        let dir = tmp("worker-nonode");
        let cfg = dir.join("w.toml");
        std::fs::write(&cfg, MIN_FLEET).unwrap();
        let args: Vec<String> =
            ["--config".to_string(), cfg.to_string_lossy().to_string()].to_vec();
        assert_eq!(run("worker", &args), 1);
    }

    #[test]
    fn run_dispatches_verbs_and_reports_errors() {
        let dir = bundle("run-dispatch");
        let out = dir.join("out.toml");
        let args: Vec<String> = [
            "deploy",
            "--dir",
            dir.to_str().unwrap(),
            "--name",
            "ci-gate",
            "--config",
            out.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // run() prints to stdout — assert via the returned code path
        // by calling deploy directly is covered above; here check the
        // verb table routes correctly for a failing verb.
        let bad = run("bogus", &[]);
        assert_eq!(bad, 1);
        let missing = run("deploy", &[]);
        assert_eq!(missing, 1);
        let _ = args;
    }

    #[test]
    fn config_schema_is_draft_2020_12() {
        let s = config_schema().unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        assert!(v["$schema"].as_str().unwrap().contains("2020-12"));
        assert_eq!(v["type"], "object");
        assert!(v["properties"]["repo"].is_object());
        assert!(v["properties"]["tunnel"]["properties"]["public_url"].is_object());
        assert!(v["properties"]["trigger"]["properties"]["listen"].is_object());
    }

    #[test]
    fn provider_units_match_tamias_naming() {
        let manifest = load_manifest(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../libraries/ci-gate")
                .as_path(),
        )
        .unwrap();
        let units = provider_units(&manifest, "ci-gate");
        assert_eq!(
            units,
            vec![
                ("watch".to_string(), "bupha-watch.service".to_string()),
                ("worker".to_string(), "bupha-worker.service".to_string()),
            ]
        );
    }

    /// The shipped ci-gate bundle must keep validating end-to-end:
    /// manifest parses, fleet merges, and the result loads as a real
    /// `bupha_core::Config`.
    #[test]
    fn shipped_ci_gate_bundle_deploys() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../libraries/ci-gate");
        let out_dir = tmp("shipped-ci-gate");
        let out = out_dir.join("ci-gate.toml");
        let line = deploy(&dir, "ci-gate", Some(&out)).unwrap();
        assert!(line.contains("repo entries"), "{line}");
        let cfg = bupha_config_file::FileSource {
            path: Some(out.to_string_lossy().to_string()),
        }
        .load()
        .unwrap();
        assert_eq!(cfg.tunnel.public_url, "https://ci.luisfuturist.com");
        assert_eq!(cfg.trigger.listen, "127.0.0.1:29173");
        // T-064: fleet.toml's [worker] block flows through deploy
        // verbatim — the worker service reads it from this config.
        assert_eq!(cfg.worker.node.as_deref(), Some("http://127.0.0.1:18787"));
        assert_eq!(cfg.worker.node_token_env.as_deref(), Some("PEREGRIN_TOKEN"));
        assert!(!cfg.repos.is_empty());
        for r in &cfg.repos {
            assert!(r.secret_env.is_some(), "{}: secret_env", r.name);
        }
    }
}
