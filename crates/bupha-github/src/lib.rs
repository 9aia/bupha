//! bupha-github — the GitHub impl of Trigger + StatusSink: verifies
//! X-Hub-Signature-256, parses push payloads, and posts commit
//! statuses to api.github.com.

use bupha_core::is_valid_sha;
use bupha_status::StatusSink;
use bupha_trigger::{PushOutcome, Trigger};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::Duration;

/// Outbound GitHub API calls get bounded so a stalled connection
/// can't hang the queue worker or the webhook handler.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .new_agent()
}

pub struct GitHubTrigger {
    pub secret: String,
}

impl Trigger for GitHubTrigger {
    fn is_event(&self, headers: &[(String, String)]) -> bool {
        headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("x-github-event") && v == "push")
    }

    fn verify(&self, body: &[u8], headers: &[(String, String)]) -> bool {
        let Some(sig) = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-hub-signature-256"))
        else {
            return false;
        };
        let Some(hex_sig) = sig.1.strip_prefix("sha256=") else {
            return false;
        };
        let Ok(sig) = hex::decode(hex_sig) else {
            return false;
        };
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(self.secret.as_bytes())
            .expect("hmac accepts any key length");
        mac.update(body);
        mac.verify_slice(&sig).is_ok()
    }

    /// (repository.full_name, ref, sha) — the watcher routes by repo
    /// name and filters the ref per watched repo.
    fn delivery_id(&self, headers: &[(String, String)]) -> Option<String> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-github-delivery"))
            .map(|(_, v)| v.clone())
    }

    fn parse_push(&self, body: &[u8]) -> Option<PushOutcome> {
        let v: serde_json::Value = serde_json::from_slice(body).ok()?;
        let repo = v.pointer("/repository/full_name")?.as_str()?;
        let git_ref = v.get("ref")?.as_str()?;
        let sha = v.get("after")?.as_str()?;
        // Branch deletions arrive with after = "000…" (and deleted:true)
        // — well-formed, but there is nothing to build.
        if v.get("deleted").and_then(|d| d.as_bool()).unwrap_or(false)
            || sha.bytes().all(|b| b == b'0')
        {
            return Some(PushOutcome::Skip);
        }
        // Signed but malformed for GitHub's contract — reject as a bad
        // payload so it shows up in the delivery log, never enqueued.
        if !is_valid_sha(sha) {
            return None;
        }
        Some(PushOutcome::Push {
            repo: repo.to_string(),
            git_ref: git_ref.to_string(),
            sha: sha.to_string(),
        })
    }
}

pub struct GitHubStatus {
    pub token: String,
    /// owner/repo
    pub repo: String,
    pub context: String,
    /// Forge API base ("https://api.github.com" or GHES).
    pub api_base: String,
}

impl StatusSink for GitHubStatus {
    fn context(&self) -> &str {
        &self.context
    }

    fn post(&self, sha: &str, state: &str, desc: &str) {
        let body = serde_json::json!({
            "state": state,
            "context": self.context,
            "description": desc,
        });
        let url = format!("{}/repos/{}/statuses/{sha}", self.api_base, self.repo);
        let res = agent()
            .post(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "bupha")
            .send_json(body);
        if let Err(e) = res {
            eprintln!("status post failed ({state} {sha:.12}): {e}");
        }
    }
}

/// No-op sink — statuses go to stdout. Useful for local runs and forges
/// without a status API.
pub struct LogStatus {
    pub context: String,
}

impl StatusSink for LogStatus {
    fn context(&self) -> &str {
        &self.context
    }

    fn post(&self, sha: &str, state: &str, desc: &str) {
        println!("[status:{}] {state} {sha:.12} {desc}", self.context);
    }
}

/// Delete the webhook pointing at `{url}/hook` — exact URL match only,
/// mirroring register_webhook. Returns a human message.
pub fn unregister_webhook(token: &str, repo: &str, url: &str, api_base: &str) -> std::io::Result<String> {
    let hook_url = format!("{}/hook", url.trim_end_matches('/'));
    let agent = agent();
    let list = agent
        .get(&format!("{api_base}/repos/{repo}/hooks"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "bupha")
        .call();
    let existing = match list {
        Ok(mut r) => {
            let v: serde_json::Value = r.body_mut().read_json().unwrap_or(serde_json::json!([]));
            v.as_array().and_then(|a| {
                a.iter().find_map(|h| {
                    let id = h.get("id")?.as_u64()?;
                    let u = h.pointer("/config/url")?.as_str()?;
                    (u.trim_end_matches('/') == hook_url).then_some(id)
                })
            })
        }
        Err(e) => {
            // Auth/scope failures shouldn't silently become "no hook"
            // and fall through to a confusing POST error.
            if e.to_string().contains("401") || e.to_string().contains("403") {
                return Err(std::io::Error::other(format!(
                    "{e} — PAT needs repo + admin:repo_hook scopes"
                )));
            }
            None
        }
    };
    match existing {
        Some(id) => {
            agent
                .delete(&format!("{api_base}/repos/{repo}/hooks/{id}"))
                .header("Authorization", &format!("Bearer {token}"))
                .header("Accept", "application/vnd.github+json")
                .header("User-Agent", "bupha")
                .call()
                .map_err(std::io::Error::other)?;
            Ok(format!("deleted hook {id} ({hook_url})"))
        }
        None => Ok(format!("no hook for {hook_url}")),
    }
}

/// Register or update the push webhook via the REST API using the
/// token directly (no `gh` dependency). Returns a human message.
pub fn register_webhook(
    token: &str,
    repo: &str,
    url: &str,
    secret: &str,
    api_base: &str,
) -> std::io::Result<String> {
    let hook_url = format!("{}/hook", url.trim_end_matches('/'));
    let agent = agent();
    let list = agent
        .get(&format!("{api_base}/repos/{repo}/hooks"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "bupha")
        .call();
    // Only take over a hook already pointing at this exact URL —
    // anything looser would hijack another service's webhook.
    let existing = match list {
        Ok(mut r) => {
            let v: serde_json::Value = r.body_mut().read_json().unwrap_or(serde_json::json!([]));
            v.as_array().and_then(|a| {
                a.iter().find_map(|h| {
                    let id = h.get("id")?.as_u64()?;
                    let u = h.pointer("/config/url")?.as_str()?;
                    (u.trim_end_matches('/') == hook_url).then_some(id)
                })
            })
        }
        Err(e) => {
            // Auth/scope failures shouldn't silently become "no hook"
            // and fall through to a confusing POST error.
            if e.to_string().contains("401") || e.to_string().contains("403") {
                return Err(std::io::Error::other(format!(
                    "{e} — PAT needs repo + admin:repo_hook scopes"
                )));
            }
            None
        }
    };
    let config = serde_json::json!({
        "url": hook_url, "content_type": "json", "secret": secret,
    });
    if let Some(id) = existing {
        agent
            .patch(&format!("{api_base}/repos/{repo}/hooks/{id}"))
            .header("Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "bupha")
            .send_json(serde_json::json!({
                "config": config, "events": ["push"], "active": true}))
            .map_err(std::io::Error::other)?;
        Ok(format!("updated hook {id} -> {hook_url}"))
    } else {
        agent
            .post(&format!("{api_base}/repos/{repo}/hooks"))
            .header("Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "bupha")
            .send_json(serde_json::json!({
                "name": "web", "config": config,
                "events": ["push"], "active": true}))
            .map_err(std::io::Error::other)?;
        Ok(format!("created hook -> {hook_url}"))
    }
}

/// `ForgeAdmin` impl — manages webhook lifecycle via the GitHub
/// REST API for one repo.
pub struct GitHubAdmin {
    /// "owner/repo" this admin manages.
    pub repo: String,
    /// Forge API base ("https://api.github.com" or GHES).
    pub api_base: String,
}

impl bupha_trigger::ForgeAdmin for GitHubAdmin {
    fn register_hook(&self, url: &str, secret: &str, token: &str) -> Result<String, String> {
        register_webhook(token, &self.repo, url, secret, &self.api_base).map_err(|e| e.to_string())
    }
    fn remove_hook(&self, url: &str, token: &str) -> Result<String, String> {
        unregister_webhook(token, &self.repo, url, &self.api_base).map_err(|e| e.to_string())
    }
    fn repo(&self) -> &str {
        &self.repo
    }
}
