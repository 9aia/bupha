use bupha_trigger::{PushOutcome, Trigger};
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
    let sha40 = "a".repeat(40);
    let body = BODY.replace("abc123def456", &sha40);
    match t.parse_push(body.as_bytes()).unwrap() {
        PushOutcome::Push { repo, git_ref, sha } => assert_eq!(
            (repo.as_str(), git_ref.as_str(), sha.as_str()),
            ("o/r", "refs/heads/main", sha40.as_str())),
        PushOutcome::Skip => panic!("expected Push"),
    }
    assert!(t.parse_push(b"{}").is_none());
    assert!(t.parse_push(b"not json").is_none());
}

#[test]
fn parse_push_skips_deleted_branch() {
    let t = GitHubTrigger { secret: "s".into() };
    // GitHub: deleted:true + after = all zeros
    let body = r#"{"ref":"refs/heads/main","deleted":true,
      "after":"0000000000000000000000000000000000000000",
      "repository":{"full_name":"o/r"}}"#;
    assert!(matches!(t.parse_push(body.as_bytes()),
        Some(PushOutcome::Skip)));
    // zero sha without deleted flag also skips
    let body = r#"{"ref":"refs/heads/main",
      "after":"0000000000000000000000000000000000000000",
      "repository":{"full_name":"o/r"}}"#;
    assert!(matches!(t.parse_push(body.as_bytes()),
        Some(PushOutcome::Skip)));
}

#[test]
fn parse_push_rejects_non_sha() {
    let t = GitHubTrigger { secret: "s".into() };
    // path-injection attempt in `after` — malformed per GitHub's
    // contract -> None (the watcher answers 400), never enqueued
    let body = r#"{"ref":"refs/heads/main","after":"../../etc",
      "repository":{"full_name":"o/r"}}"#;
    assert!(t.parse_push(body.as_bytes()).is_none());
    let body = r#"{"ref":"refs/heads/main","after":"zzzz",
      "repository":{"full_name":"o/r"}}"#;
    assert!(t.parse_push(body.as_bytes()).is_none());
}

#[test]
fn delivery_id_header() {
    let t = bupha_github::GitHubTrigger { secret: "s".into() };
    let h = vec![("X-GitHub-Delivery".to_string(), "guid-7".to_string())];
    assert_eq!(t.delivery_id(&h).as_deref(), Some("guid-7"));
    assert_eq!(t.delivery_id(&[]), None);
}
