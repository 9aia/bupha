//! LocalClaim: the single-box lease — always claims, never loses;
//! its in-memory done set behaves like a peer-completed marker.

use bupha_queue_lease::{Fence, LocalClaim, TryClaim};

#[test]
fn local_claim_always_claims_and_renews() {
    let l = LocalClaim::default();
    let f = l.try_claim("repo", "sha", "w1").unwrap();
    assert!(f.is_some());
    assert_eq!(f.unwrap().0, 0);
    assert!(l.renew("repo", "sha", "w1").unwrap());
}

#[test]
fn local_claim_done_set() {
    let l = LocalClaim::default();
    assert!(!l.is_done("repo", "sha").unwrap());
    l.mark_done("repo", "sha").unwrap();
    assert!(l.is_done("repo", "sha").unwrap());
    assert!(!l.is_done("repo", "other").unwrap());
    assert!(!l.is_done("other-repo", "sha").unwrap());
}

#[test]
fn trait_defaults_for_minimal_impls() {
    // An impl that only does try_claim inherits always-renew,
    // never-done, noop-mark — the single-box defaults.
    struct Bare;
    impl TryClaim for Bare {
        fn try_claim(&self, _: &str, _: &str, _: &str) -> std::io::Result<Option<Fence>> {
            Ok(Some(Fence(1)))
        }
    }
    let b = Bare;
    assert!(b.renew("r", "s", "w").unwrap());
    assert!(!b.is_done("r", "s").unwrap());
    b.mark_done("r", "s").unwrap();
    assert!(!b.is_done("r", "s").unwrap());
}
