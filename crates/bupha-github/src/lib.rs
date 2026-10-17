//! bupha-github — the GitHub impl of Trigger + StatusSink: verifies
//! X-Hub-Signature-256, parses push payloads, and posts commit
//! statuses to api.github.com.

use bupha_core::{StatusSink, Trigger};
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub struct GitHubTrigger {
    pub secret: String,
}

impl Trigger for GitHubTrigger {
    fn is_event(&self, headers: &[(String, String)]) -> bool {
        headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("x-github-event") && v == "push"
        })
    }

    fn verify(&self, body: &[u8], headers: &[(String, String)]) -> bool {
        let Some(sig) = headers.iter().find(|(k, _)| {
            k.eq_ignore_ascii_case("x-hub-signature-256")
        }) else {
            return false;
        };
        let Some(hex_sig) = sig.1.strip_prefix("sha256=") else {
            return false;
        };
        let Ok(sig) = hex::decode(hex_sig) else { return false };
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(
            self.secret.as_bytes())
        .expect("hmac accepts any key length");
        mac.update(body);
        mac.verify_slice(&sig).is_ok()
    }

    /// (repository.full_name, ref, sha) — the watcher routes by repo
    /// name and filters the ref per watched repo.
    fn parse_push(&self, body: &[u8])
            -> Option<(String, String, String)> {
        let v: serde_json::Value = serde_json::from_slice(body).ok()?;
        let repo = v.pointer("/repository/full_name")?.as_str()?;
        let git_ref = v.get("ref")?.as_str()?;
        let sha = v.get("after")?.as_str()?;
        Some((repo.to_string(), git_ref.to_string(), sha.to_string()))
    }
}

pub struct GitHubStatus {
    pub token: String,
    /// owner/repo
    pub repo: String,
    pub context: String,
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
        let url = format!(
            "https://api.github.com/repos/{}/statuses/{sha}", self.repo);
        let res = ureq::post(&url)
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

/// Register or update the push webhook via the REST API using the
/// token directly (no `gh` dependency). Returns a human message.
pub fn register_webhook(
    token: &str, repo: &str, url: &str, secret: &str,
) -> std::io::Result<String> {
    let hook_url = format!("{url}/hook");
    let list = ureq::get(&format!(
            "https://api.github.com/repos/{repo}/hooks"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "bupha")
        .call();
    let existing = match list {
        Ok(mut r) => {
            let v: serde_json::Value = r.body_mut().read_json()
                .unwrap_or(serde_json::json!([]));
            v.as_array().and_then(|a| a.iter().find_map(|h| {
                let id = h.get("id")?.as_u64()?;
                let u = h.pointer("/config/url")?.as_str()?;
                u.ends_with("/hook").then_some(id)
            }))
        }
        Err(_) => None,
    };
    let config = serde_json::json!({
        "url": hook_url, "content_type": "json", "secret": secret,
    });
    if let Some(id) = existing {
        ureq::patch(&format!(
                "https://api.github.com/repos/{repo}/hooks/{id}"))
            .header("Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "bupha")
            .send_json(serde_json::json!({
                "config": config, "events": ["push"], "active": true}))
            .map_err(std::io::Error::other)?;
        Ok(format!("updated hook {id} -> {hook_url}"))
    } else {
        ureq::post(&format!(
            "https://api.github.com/repos/{repo}/hooks"))
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
