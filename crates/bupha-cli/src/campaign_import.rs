//! `bupha import campaign` — translate a peregrin campaign YAML
//! (the ci-gate fleet shape: a `github-webhook` node carrying
//! `params.repos[]`, `git` job nodes with `on: push.<repo>.<ref>…`
//! topics and a shell `inner` cmd, `github-status` nodes carrying
//! the status contexts) into a `bupha.yaml` fleet config.
//!
//! Mapping: webhook `params.repos[]` → `repos[]` entries
//! (`secret_env` names pass through; the majority secret becomes
//! `trigger.secret_env`); each `git` node → one `jobs[]` entry
//! (`branch` from the `on:` ref, `runner.kind = "shell"` + the
//! inner `cmd`, `context` from the matching `github-status` node);
//! single-job repos whose context is the default collapse to the
//! implicit repo form (repo-level `branch`/`runner`).

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// One `git` job node, normalized.
#[derive(Clone)]
struct Job {
    /// Node id ("ci.o-r-docs") — the job-name source.
    id: String,
    /// repository.full_name.
    repo: String,
    /// Full ref ("refs/heads/main").
    git_ref: String,
    /// Shell command from `params.inner.params.cmd`.
    cmd: String,
    /// Status context from the matching github-status node.
    context: String,
}

/// `owner/name` → local clone path, resolved per repo.
fn repo_dir(repo: &str, jobs: &[Job], overrides: &HashMap<String, String>) -> (String, bool) {
    if let Some(d) = overrides.get(repo) {
        return (d.clone(), true);
    }
    // A cmd carrying an absolute path inside the clone reveals the
    // local layout ("bash /home/x/GitHub/o/r/scripts/ci.sh") — the
    // path starts after the last whitespace before the match.
    let needle = format!("/{repo}/");
    for j in jobs {
        if let Some(i) = j.cmd.find(&needle) {
            let end = i + needle.len() - 1;
            let start = j.cmd[..i]
                .rfind(char::is_whitespace)
                .map(|w| w + 1)
                .unwrap_or(0);
            let dir = &j.cmd[start..end];
            if dir.starts_with('/') {
                return (dir.to_string(), true);
            }
        }
    }
    (format!("~/GitHub/{repo}"), false)
}

/// `ci.<slug>` node id → job name: strip the repo slug prefix
/// ("ci.9aia-bupha-docs" → "docs"); the bare slug falls back to the
/// branch's short name. Always path-safe ([A-Za-z0-9._-]).
fn job_name(id: &str, repo: &str, git_ref: &str) -> String {
    let slug = repo.replace('/', "-");
    let body = id.strip_prefix("ci.").unwrap_or(id);
    let rest = body
        .strip_prefix(&format!("{slug}-"))
        .or_else(|| (body == slug).then_some(""))
        .unwrap_or(body);
    let short = git_ref.rsplit('/').next().unwrap_or(git_ref);
    let raw = if rest.is_empty() { short } else { rest };
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_matches('-');
    if clean.is_empty() {
        "job".into()
    } else {
        clean.to_string()
    }
}

/// `push.<repo>.<ref>.<wildcards>` topic → (repo, ref). The ref may
/// itself contain dots, so the wildcard tail (`**`/`*` segments) is
/// what delimits it.
fn parse_push_topic(topic: &str) -> Option<(String, String)> {
    let rest = topic.strip_prefix("push.")?;
    let (repo, tail) = rest.split_once('.')?;
    // The ref runs to the first wildcard segment.
    let end = tail
        .find("**")
        .map(|i| i.saturating_sub(1))
        .unwrap_or(tail.len());
    let git_ref = tail[..end].trim_end_matches('.').to_string();
    if repo.is_empty() || git_ref.is_empty() {
        return None;
    }
    Some((repo.to_string(), git_ref))
}

/// YAML double-quoted scalar — JSON string escaping is valid YAML.
fn yq(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

fn emit_runner(out: &mut String, indent: &str, cmd: &str) {
    out.push_str(&format!(
        "{indent}runner:\n{indent}  kind: shell\n{indent}  cmd: {}\n{indent}  cancel_running: true\n",
        yq(cmd)
    ));
}

/// Translate campaign YAML text → (bupha.yaml text, repo count,
/// guessed-dir count).
pub fn campaign_to_yaml(
    text: &str,
    dir_overrides: &HashMap<String, String>,
    with_pull_request: bool,
) -> Result<(String, usize, usize), String> {
    let v: Value = serde_norway::from_str(text).map_err(|e| format!("parse campaign yaml: {e}"))?;
    let nodes = v
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("campaign has no nodes[]")?;

    // Webhook node → listener + per-repo secret names.
    let mut listen = "127.0.0.1:29173".to_string();
    let mut secrets: HashMap<String, String> = HashMap::new();
    for n in nodes {
        if n.get("driver").and_then(Value::as_str) != Some("github-webhook") {
            continue;
        }
        if let Some(l) = n.pointer("/params/listen").and_then(Value::as_str) {
            listen = l.to_string();
        }
        if let Some(rs) = n.pointer("/params/repos").and_then(Value::as_array) {
            for r in rs {
                let (Some(name), Some(sec)) = (
                    r.get("name").and_then(Value::as_str),
                    r.get("secret_env").and_then(Value::as_str),
                ) else {
                    continue;
                };
                secrets.insert(name.to_string(), sec.to_string());
            }
        }
    }
    // The most common secret_env across repos becomes the global
    // `trigger.secret_env` fallback; per-repo overrides emit only
    // when they differ.
    let mut freq: BTreeMap<&str, usize> = BTreeMap::new();
    for s in secrets.values() {
        *freq.entry(s.as_str()).or_default() += 1;
    }
    let global_secret = freq
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(s, _)| s.to_string())
        .unwrap_or_else(|| "BUPHA_WEBHOOK_SECRET".into());

    // Status contexts: (repo, ref) → context, from github-status
    // nodes whose `on:` matches a push topic.
    let mut contexts: HashMap<(String, String), String> = HashMap::new();
    for n in nodes {
        if n.get("driver").and_then(Value::as_str) != Some("github-status") {
            continue;
        }
        let Some(ctx) = n.pointer("/params/context").and_then(Value::as_str) else {
            continue;
        };
        let Some(ons) = n.get("on").and_then(Value::as_array) else {
            continue;
        };
        for on in ons {
            if let Some(t) = on.as_str().and_then(parse_push_topic) {
                contexts.entry(t).or_insert_with(|| ctx.to_string());
            }
        }
    }

    // Job nodes.
    let mut jobs: Vec<Job> = Vec::new();
    for n in nodes {
        if n.get("driver").and_then(Value::as_str) != Some("git") {
            continue;
        }
        let Some(id) = n.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(ons) = n.get("on").and_then(Value::as_array) else {
            continue;
        };
        let Some((repo, git_ref)) = ons
            .iter()
            .filter_map(Value::as_str)
            .find_map(parse_push_topic)
        else {
            continue;
        };
        let cmd = n
            .pointer("/params/inner/params/cmd")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("node {id}: git job without inner.params.cmd"))?;
        let context = contexts
            .get(&(repo.clone(), git_ref.clone()))
            .cloned()
            .unwrap_or_else(|| "ci/bupha".into());
        jobs.push(Job {
            id: id.to_string(),
            repo,
            git_ref,
            cmd: cmd.to_string(),
            context,
        });
    }
    if jobs.is_empty() {
        return Err("no git job nodes with push.<repo>.<ref> topics found".into());
    }

    // Group jobs by repo; union with webhook-only repos.
    let mut by_repo: BTreeMap<String, Vec<Job>> = BTreeMap::new();
    for j in jobs {
        by_repo.entry(j.repo.clone()).or_default().push(j);
    }
    let mut repo_names: Vec<String> = by_repo.keys().cloned().collect();
    for name in secrets.keys() {
        if !by_repo.contains_key(name) {
            repo_names.push(name.clone());
        }
    }
    repo_names.sort();

    let mut out = String::new();
    out.push_str("# bupha.yaml — generated by `bupha import campaign` (peregrin\n");
    out.push_str("# campaign → bupha fleet config). Review `dir` values and\n");
    out.push_str("# secret resolution before `bupha deploy service`.\n\n");
    out.push_str("trigger:\n");
    out.push_str("  kind: github\n");
    out.push_str(&format!("  listen: {}\n", yq(&listen)));
    out.push_str(&format!("  secret_env: {}\n\n", yq(&global_secret)));
    if with_pull_request {
        out.push_str("# every repo below also runs on pull_request deliveries\n");
        out.push_str("# (re-run `deploy webhook` so hooks subscribe to both)\n\n");
    }
    out.push_str("status:\n  kind: github\n  context: \"ci/bupha\"\n\n");
    // `runner` is a required top-level field — every imported job
    // carries its own, so this is only the schema fallback.
    out.push_str("runner:\n  kind: shell\n  cmd: \"bash ci.sh\"\n\n");
    out.push_str("worktree:\n  base: \"~/.local/share/bupha/worktrees\"\n\n");
    out.push_str("repos:\n");

    let mut guessed = 0usize;
    for name in &repo_names {
        let jobs = by_repo.get(name).cloned().unwrap_or_default();
        let (dir, known) = repo_dir(name, &jobs, dir_overrides);
        if !known {
            guessed += 1;
        }
        let sec = secrets.get(name).filter(|s| s.as_str() != global_secret);
        out.push_str(&format!("  - name: {}\n    dir: {}\n", yq(name), yq(&dir)));
        if let Some(s) = sec {
            out.push_str(&format!("    secret_env: {}\n", yq(s)));
        }
        if with_pull_request {
            out.push_str("    events: [push, pull_request]\n");
        }
        match jobs.len() {
            // Implicit single-pipeline form when the context is the
            // global default — repo-level branch + runner carry it.
            1 if jobs[0].context == "ci/bupha" => {
                out.push_str(&format!("    branch: {}\n", yq(&jobs[0].git_ref)));
                emit_runner(&mut out, "    ", &jobs[0].cmd);
            }
            0 => {}
            _ => {
                out.push_str("    jobs:\n");
                for j in &jobs {
                    out.push_str(&format!(
                        "      - name: {}\n        branch: {}\n        context: {}\n",
                        yq(&job_name(&j.id, name, &j.git_ref)),
                        yq(&j.git_ref),
                        yq(&j.context)
                    ));
                    emit_runner(&mut out, "        ", &j.cmd);
                }
            }
        }
    }

    Ok((out, repo_names.len(), guessed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAMPAIGN: &str = r#"
name: mini-gate
nodes:
- id: ci.ingress.webhook
  driver: github-webhook
  on: [start]
  params:
    listen: 127.0.0.1:29173
    repos:
    - name: o/one
      secret_env: BUPHA_WH_O_ONE
    - name: o/two
      secret_env: BUPHA_WEBHOOK_SECRET
- id: ci.route
  driver: shell
  on: [push]
  params:
    cmd: 'true'
- id: ci.o-one
  driver: git
  on:
  - push.o/one.refs/heads/main.**.*
  params:
    inner:
      driver: shell
      params:
        cmd: bash /srv/git/o/one/ci.sh
        timeout: 600
- id: ci.o-one.pending
  driver: github-status
  on:
  - push.o/one.refs/heads/main.**.*
  params:
    repo: o/one
    context: ci/bupha
    state: pending
- id: ci.o-two-docs
  driver: git
  on:
  - push.o/two.refs/heads/docs.**.*
  params:
    inner:
      driver: shell
      params:
        cmd: bash scripts/deploy-docs.sh
- id: ci.o-two-docs.pending
  driver: github-status
  on:
  - push.o/two.refs/heads/docs.**.*
  params:
    repo: o/two
    context: ci/bupha/docs
    state: pending
- id: ci.o-two-main
  driver: git
  on:
  - push.o/two.refs/heads/main.**.*
  params:
    inner:
      driver: shell
      params:
        cmd: bash ci.sh
- id: ci.o-two-main.pending
  driver: github-status
  on:
  - push.o/two.refs/heads/main.**.*
  params:
    repo: o/two
    context: ci/bupha/main
    state: pending
"#;

    #[test]
    fn translates_repos_jobs_contexts() {
        let (yaml, repos, _) = campaign_to_yaml(CAMPAIGN, &HashMap::new(), false).unwrap();
        assert_eq!(repos, 2);
        assert!(yaml.contains("kind: github"));
        // The two secrets tie 1-1 — the majority vote picks one as
        // `trigger.secret_env`, the other emits once as a per-repo
        // override. Exactly two secret_env lines total.
        assert_eq!(yaml.matches("secret_env:").count(), 2);
        // o/one: single job, default context -> implicit form with a
        // cmd-derived dir.
        assert!(yaml.contains("dir: \"/srv/git/o/one\""));
        assert!(yaml.contains("branch: \"refs/heads/main\""));
        assert!(yaml.contains("cmd: \"bash /srv/git/o/one/ci.sh\""));
        // o/two: two jobs -> jobs list with per-job contexts.
        assert!(yaml.contains("jobs:"));
        assert!(yaml.contains("- name: \"docs\""));
        assert!(yaml.contains("- name: \"main\""));
        assert!(yaml.contains("context: \"ci/bupha/docs\""));
        assert!(yaml.contains("context: \"ci/bupha/main\""));
    }

    #[test]
    fn pull_request_flag_emits_events() {
        let (yaml, _, _) = campaign_to_yaml(CAMPAIGN, &HashMap::new(), true).unwrap();
        assert_eq!(yaml.matches("events: [push, pull_request]").count(), 2);
    }

    #[test]
    fn dir_override_wins() {
        let mut m = HashMap::new();
        m.insert("o/one".to_string(), "/elsewhere/one".to_string());
        let (yaml, _, _) = campaign_to_yaml(CAMPAIGN, &m, false).unwrap();
        assert!(yaml.contains("dir: \"/elsewhere/one\""));
    }

    #[test]
    fn rejects_garbage() {
        assert!(campaign_to_yaml("not: [valid", &HashMap::new(), false).is_err());
        assert!(campaign_to_yaml("nodes: []", &HashMap::new(), false).is_err());
    }

    /// Real fleet file — `cargo test -p bupha campaign_import -- --ignored`.
    #[test]
    #[ignore]
    fn real_ci_gate_campaign() {
        let path = "/home/luis/GitHub/9aia/workspace/repos/peregrin/libraries/ci-gate/main.yaml";
        let text = std::fs::read_to_string(path).unwrap();
        let (yaml, repos, _) = campaign_to_yaml(&text, &HashMap::new(), false).unwrap();
        eprintln!("repos={repos} yaml_bytes={}", yaml.len());
        assert!(repos > 100);
    }
}
