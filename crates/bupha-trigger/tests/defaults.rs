//! Port defaults: `delivery_id`/`is_ping` are optional — a trigger
//! without the concept returns None/false.

use bupha_trigger::{PushOutcome, Trigger};

struct Nop;

impl Trigger for Nop {
    fn is_event(&self, _headers: &[(String, String)]) -> bool {
        false
    }
    fn repo_hint(&self, _body: &[u8]) -> Option<String> {
        None
    }
    fn verify(&self, _secret: &str, _body: &[u8], _headers: &[(String, String)]) -> bool {
        false
    }
    fn parse_push(&self, _body: &[u8], _headers: &[(String, String)]) -> Option<PushOutcome> {
        None
    }
}

#[test]
fn default_delivery_id_is_none() {
    let t = Nop;
    assert!(t
        .delivery_id(&[("X-GitHub-Delivery".into(), "d".into())])
        .is_none());
}

#[test]
fn default_is_ping_is_false() {
    let t = Nop;
    assert!(!t.is_ping(&[("X-GitHub-Event".into(), "ping".into())]));
}
