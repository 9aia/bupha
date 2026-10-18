//! File adapter of `Access` — a global allowlist of forge logins
//! from `[access] allow = [...]`. First-impl convenience; the proper
//! adapter is `bupha-access-forge` (role check via the forge API).

use bupha_access::Access;

/// Allowlist of forge logins. Empty list = deny nobody? No —
/// an empty list denies everyone; `AllowAll` is the explicit open
/// policy. `repo` is ignored — the allowlist is global.
pub struct FileAccess {
    /// Forge logins permitted to trigger any watched repo.
    pub allow: Vec<String>,
}

impl Access for FileAccess {
    fn allow(&self, sender: &str, _repo: &str) -> bool {
        self.allow.iter().any(|u| u == sender)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_matches_sender() {
        let a = FileAccess {
            allow: vec!["alice".into(), "ci-bot".into()],
        };
        assert!(a.allow("alice", "org/repo"));
        assert!(a.allow("ci-bot", "org/repo"));
        assert!(!a.allow("mallory", "org/repo"));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let a = FileAccess { allow: vec![] };
        assert!(!a.allow("alice", "org/repo"));
    }

    #[test]
    fn allow_all_is_open() {
        assert!(bupha_access::AllowAll.allow("mallory", "org/repo"));
    }
}
