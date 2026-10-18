//! Forge impl of `Access` — "does the pusher have write access to
//! this repo?" via the forge's collaborator-permission API. Same
//! trust model hosted CI uses: policy lives in the forge, not a file.
//!
//! github impl: `GET /repos/{repo}/collaborators/{user}/permission`
//! (200 = allowed, 404 = not a collaborator → deny).

use bupha_access::Access;
use std::collections::HashMap;
use std::sync::Mutex;

/// Checks the forge's collaborator permission for (sender, repo) —
/// caches positive results in-process (a collaborator doesn't lose
/// write mid-run realistically).
pub struct ForgeAccess {
    pub api_base: String,
    pub token: String,
    pub min_role: String,
    cache: Mutex<HashMap<(String, String), bool>>,
}

impl ForgeAccess {
    pub fn new(api_base: &str, token: &str) -> Self {
        Self {
            api_base: api_base.to_string(),
            token: token.to_string(),
            min_role: "push".into(),
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl Access for ForgeAccess {
    fn allow(&self, sender: &str, repo: &str) -> bool {
        let key = (sender.to_string(), repo.to_string());
        if let Some(&ok) = self.cache.lock().unwrap().get(&key) {
            return ok;
        }
        // github: GET collaborator permission → role ∈ {admin, write}
        let url = format!(
            "{}/repos/{}/collaborators/{}/permission",
            self.api_base, repo, sender
        );
        let ok = ureq::get(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "bupha")
            .call()
            .map(|r| {
                let body: serde_json::Value = r.into_body().read_json().unwrap_or_default();
                let p = body
                    .get("permission")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                matches!(p, "admin" | "write" | "maintain")
            })
            .unwrap_or(false);
        self.cache.lock().unwrap().insert(key, ok);
        ok
    }
}
