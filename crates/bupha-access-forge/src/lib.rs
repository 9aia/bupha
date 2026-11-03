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

/// Percent-encode one URL path segment — sender/repo can come from
/// signed forge payloads (well-formed logins) or self-asserted
/// source identities (sturn actors, arbitrary strings). Unreserved
/// chars only, everything else is %-escaped so a hostile value can't
/// reshape the request path or smuggle a query.
fn encode_segment(s: &str) -> String {
    // "." and ".." are legal unreserved chars but dangerous as whole
    // path segments — force-encode them.
    if s == "." || s == ".." {
        return s.bytes().map(|b| format!("%{b:02X}")).collect();
    }
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
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
            self.api_base,
            // "owner/name" is two path segments — encode each.
            repo.split('/')
                .map(encode_segment)
                .collect::<Vec<_>>()
                .join("/"),
            encode_segment(sender)
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
        // Only positives are cached — a denied sender may legitimately
        // gain access later (invite accepted), and a transient API
        // failure must not pin a false deny for the process's life.
        if ok {
            self.cache.lock().unwrap().insert(key, true);
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::encode_segment;

    #[test]
    fn segment_encoding() {
        assert_eq!(encode_segment("alice"), "alice");
        assert_eq!(encode_segment("a-b_c.d~e"), "a-b_c.d~e");
        // "../x" is ONE segment — "/" encodes, leaving ".." harmless.
        assert_eq!(encode_segment("../x"), "..%2Fx");
        // A bare ".." segment would traverse — force-encode it.
        assert_eq!(encode_segment(".."), "%2E%2E");
        assert_eq!(encode_segment("."), "%2E");
        assert_eq!(encode_segment("a?b=c"), "a%3Fb%3Dc");
        assert_eq!(encode_segment("a b"), "a%20b");
    }
}
