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

    fn parse_push(&self, body: &[u8], branch: &str) -> Option<String> {
        let v: serde_json::Value = serde_json::from_slice(body).ok()?;
        if v.get("ref")?.as_str()? != branch {
            return None;
        }
        v.get("after")?.as_str().map(str::to_string)
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
