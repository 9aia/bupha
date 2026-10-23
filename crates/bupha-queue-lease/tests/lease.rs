//! LocalClaim: the single-box lease — always claims, never loses.

use bupha_queue_lease::{LocalClaim, TryClaim};

#[test]
fn local_claim_always_claims_and_renews() {
    let l = LocalClaim;
    let f = l.try_claim("repo", "sha", "w1").unwrap();
    assert!(f.is_some());
    assert_eq!(f.unwrap().0, 0);
    assert!(l.renew("repo", "sha", "w1").unwrap());
}
