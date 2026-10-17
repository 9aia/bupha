use bupha_core::Trigger;
use bupha_github::GitHubTrigger;
use hmac::{Hmac, Mac};
use sha2::Sha256;

fn sign(secret: &str, body: &[u8]) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
    m.update(body);
    format!("sha256={}", hex::encode(m.finalize().into_bytes()))
}

const BODY: &str = r#"{"ref":"refs/heads/main","after":"abc123def456",
  "repository":{"full_name":"o/r"}}"#;

#[test]
fn verify_accepts_good_sig() {
    let t = GitHubTrigger { secret: "s".into() };
    let h = vec![("X-Hub-Signature-256".into(), sign("s", BODY.as_bytes()))];
    assert!(t.verify(BODY.as_bytes(), &h));
}

#[test]
fn verify_rejects_bad_sig() {
    let t = GitHubTrigger { secret: "s".into() };
    let h = vec![("X-Hub-Signature-256".into(), "sha256=deadbeef".into())];
    assert!(!t.verify(BODY.as_bytes(), &h));
    assert!(!t.verify(BODY.as_bytes(), &[]));
}

#[test]
fn is_event_checks_push_header() {
    let t = GitHubTrigger { secret: "s".into() };
    assert!(t.is_event(&[("X-GitHub-Event".into(), "push".into())]));
    assert!(!t.is_event(&[("X-GitHub-Event".into(), "ping".into())]));
    assert!(!t.is_event(&[]));
}

#[test]
fn parse_push_extracts_repo_ref_sha() {
    let t = GitHubTrigger { secret: "s".into() };
    let (repo, r, sha) = t.parse_push(BODY.as_bytes()).unwrap();
    assert_eq!((repo.as_str(), r.as_str(), sha.as_str()),
               ("o/r", "refs/heads/main", "abc123def456"));
    assert!(t.parse_push(b"{}").is_none());
    assert!(t.parse_push(b"not json").is_none());
}
