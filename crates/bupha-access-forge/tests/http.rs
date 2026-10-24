//! ForgeAccess against a stub HTTP API on 127.0.0.1 — covers the
//! permission mapping, positive-result cache, deny on 404/non-JSON
//! bodies, and transport-level errors.

use bupha_access::Access;
use bupha_access_forge::ForgeAccess;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Speak just enough HTTP/1.1 for ureq: read request line + headers +
/// Content-Length body, hand (request-line, body) to `f`, write the
/// status/body it returns.
fn serve(f: impl Fn(&str, &[u8]) -> (u16, String) + Send + 'static) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut reqline = String::new();
            if r.read_line(&mut reqline).is_err() {
                continue;
            }
            let mut clen = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        clen = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; clen];
            let _ = r.read_exact(&mut body);
            let (code, resp) = f(&reqline, &body);
            let _ = write!(
                s,
                "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                resp.len()
            );
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[test]
fn collaborator_with_push_role_allowed_and_cached() {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let base = serve(move |req, _| {
        h.fetch_add(1, Ordering::SeqCst);
        assert!(req.contains("/repos/o/r/collaborators/alice/permission"));
        (200, r#"{"permission":"admin"}"#.into())
    });
    let a = ForgeAccess::new(&base, "tok");
    assert!(a.allow("alice", "o/r"));
    assert!(a.allow("alice", "o/r"), "second call is a cache hit");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn read_only_collaborator_denied() {
    let base = serve(|_, _| (200, r#"{"permission":"read"}"#.into()));
    let a = ForgeAccess::new(&base, "tok");
    assert!(!a.allow("bob", "o/r"));
    // maintain is allowed too
    let base2 = serve(|_, _| (200, r#"{"permission":"maintain"}"#.into()));
    let a2 = ForgeAccess::new(&base2, "tok");
    assert!(a2.allow("bob", "o/r"));
}

#[test]
fn stranger_404_denied() {
    let base = serve(|_, _| (404, "{}".into()));
    let a = ForgeAccess::new(&base, "tok");
    assert!(!a.allow("mallory", "o/r"));
}

#[test]
fn non_json_body_denied() {
    let base = serve(|_, _| (200, "not json".into()));
    let a = ForgeAccess::new(&base, "tok");
    assert!(!a.allow("bob", "o/r"));
}

#[test]
fn transport_error_denied() {
    // Nothing listens on this port — connection refused.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let a = ForgeAccess::new(&format!("http://127.0.0.1:{port}"), "tok");
    assert!(!a.allow("bob", "o/r"));
}
