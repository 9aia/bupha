//! Live forge-permission check — needs `GH_TOKEN` + `FORGE_TEST_REPO`
//! ("owner/repo" the token can see) + `FORGE_TEST_USER` (a login with
//! write access). `cargo test -p bupha-access-forge -- --ignored`

use bupha_access::Access;
use bupha_access_forge::ForgeAccess;

#[test]
#[ignore]
fn live_collaborator_allowed() {
    let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
    let repo = std::env::var("FORGE_TEST_REPO").expect("FORGE_TEST_REPO");
    let user = std::env::var("FORGE_TEST_USER").expect("FORGE_TEST_USER");
    let a = ForgeAccess::new("https://api.github.com", &token);
    assert!(a.allow(&user, &repo), "collaborator should be allowed");
}

#[test]
#[ignore]
fn live_stranger_denied() {
    let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
    let repo = std::env::var("FORGE_TEST_REPO").expect("FORGE_TEST_REPO");
    let a = ForgeAccess::new("https://api.github.com", &token);
    assert!(
        !a.allow("definitely-not-a-real-user-zzz", &repo),
        "non-collaborator should be denied"
    );
}
