//! Webhook admin + status sink against a stub forge API on
//! 127.0.0.1 — covers register (create/update/401/5xx), unregister
//! (delete/no-match), GitHubStatus::post (ok/transport-error), and
//! LogStatus output.

use bupha_github::{GitHubAdmin, GitHubStatus, LogStatus};
use bupha_status::StatusSink;
use bupha_trigger::ForgeAdmin;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

/// Just enough HTTP/1.1 for ureq: request line + headers + body in,
/// (status, body) out. `f` sees "METHOD /path".
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
fn register_creates_when_no_match() {
    let base = serve(|req, body| {
        if req.starts_with("GET") {
            assert!(req.contains("/repos/o/r/hooks"));
            return (200, "[]".into());
        }
        assert!(req.starts_with("POST") && req.contains("/repos/o/r/hooks"));
        // send_json pretty-prints — parse instead of substring match.
        let v: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(v["name"], "web");
        assert_eq!(v["config"]["url"], "http://ci/hook");
        (201, "{}".into())
    });
    let msg = bupha_github::register_webhook("tok", "o/r", "http://ci/", "sec", &base).unwrap();
    assert!(msg.contains("created hook -> http://ci/hook"));
}

#[test]
fn register_updates_existing() {
    let base = serve(|req, _| {
        if req.starts_with("GET") {
            return (
                200,
                r#"[{"id":7,"config":{"url":"http://ci/hook"}}]"#.into(),
            );
        }
        assert!(req.starts_with("PATCH") && req.contains("/repos/o/r/hooks/7"));
        (200, "{}".into())
    });
    let msg = bupha_github::register_webhook("tok", "o/r", "http://ci", "sec", &base).unwrap();
    assert!(msg.contains("updated hook 7 -> http://ci/hook"));
}

#[test]
fn register_401_needs_scopes() {
    let base = serve(|_, _| (401, "{}".into()));
    let e = bupha_github::register_webhook("tok", "o/r", "http://ci", "sec", &base).unwrap_err();
    assert!(e.to_string().contains("admin:repo_hook"));
}

#[test]
fn register_5xx_falls_through_to_post_error() {
    let base = serve(|req, _| {
        if req.starts_with("GET") {
            return (500, "oops".into());
        }
        (500, "oops".into())
    });
    let e = bupha_github::register_webhook("tok", "o/r", "http://ci", "sec", &base).unwrap_err();
    assert!(!e.to_string().is_empty());
}

#[test]
fn register_non_json_list_then_post() {
    let base = serve(|req, _| {
        if req.starts_with("GET") {
            return (200, "garbage".into());
        }
        (201, "{}".into())
    });
    let msg = bupha_github::register_webhook("tok", "o/r", "http://ci", "sec", &base).unwrap();
    assert!(msg.contains("created hook"));
}

#[test]
fn unregister_deletes_match_and_reports_none() {
    let base = serve(|req, _| {
        if req.starts_with("GET") {
            return (
                200,
                r#"[{"id":9,"config":{"url":"http://ci/hook/"}},{"id":8,"config":{"url":"http://other/hook"}}]"#
                    .into(),
            );
        }
        assert!(req.starts_with("DELETE") && req.contains("/repos/o/r/hooks/9"));
        (204, "".into())
    });
    let msg = bupha_github::unregister_webhook("tok", "o/r", "http://ci", &base).unwrap();
    assert!(msg.contains("deleted hook 9"));

    let base2 = serve(|req, _| {
        if req.starts_with("GET") {
            return (200, "[]".into());
        }
        (500, "unexpected".into())
    });
    let msg = bupha_github::unregister_webhook("tok", "o/r", "http://ci", &base2).unwrap();
    assert!(msg.contains("no hook for http://ci/hook"));

    // transport error (not 401/403) -> treated as "no existing hook"
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let msg = bupha_github::unregister_webhook(
        "tok",
        "o/r",
        "http://ci",
        &format!("http://127.0.0.1:{port}"),
    )
    .unwrap();
    assert!(msg.contains("no hook for http://ci/hook"));
}

#[test]
fn unregister_403_needs_scopes() {
    let base = serve(|_, _| (403, "{}".into()));
    let e = bupha_github::unregister_webhook("tok", "o/r", "http://ci", &base).unwrap_err();
    assert!(e.to_string().contains("admin:repo_hook"));
}

#[test]
fn admin_trait_delegates() {
    let base = serve(|req, _| {
        if req.starts_with("GET") {
            return (200, "[]".into());
        }
        (200, "{}".into())
    });
    let a = GitHubAdmin {
        repo: "o/r".into(),
        api_base: base,
    };
    assert_eq!(a.repo(), "o/r");
    assert!(a
        .register_hook("http://ci", "sec", "tok")
        .unwrap()
        .contains("created"));
    assert!(a
        .remove_hook("http://ci", "tok")
        .unwrap()
        .contains("no hook"));

    // Err arm of the trait glue (server 401s everything)
    let base401 = serve(|_, _| (401, "{}".into()));
    let a2 = GitHubAdmin {
        repo: "o/r".into(),
        api_base: base401,
    };
    assert!(a2.register_hook("http://ci", "sec", "tok").is_err());
    assert!(a2.remove_hook("http://ci", "tok").is_err());
}

#[test]
fn status_post_ok_and_transport_error() {
    // post() swallows send errors, so capture the body and check it
    // in the test — a handler-side assert would fail invisibly.
    let got = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let g = got.clone();
    let base = serve(move |req, body| {
        assert!(req.starts_with("POST") && req.contains("/repos/o/r/statuses/abc"));
        *g.lock().unwrap() = String::from_utf8_lossy(body).into_owned();
        (201, "{}".into())
    });
    let s = GitHubStatus {
        token: "t".into(),
        repo: "o/r".into(),
        context: "ci/bupha".into(),
        api_base: base,
    };
    assert_eq!(s.context(), "ci/bupha");
    s.post("abc", "success", "ci passed");
    let v: serde_json::Value = serde_json::from_str(&got.lock().unwrap()).unwrap();
    assert_eq!(v["state"], "success");

    // nothing listening -> Err arm eprintln
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let s2 = GitHubStatus {
        token: "t".into(),
        repo: "o/r".into(),
        context: "ci".into(),
        api_base: format!("http://127.0.0.1:{port}"),
    };
    s2.post("abc", "failure", "ci failed");
}

#[test]
fn log_status_prints() {
    let s = LogStatus {
        context: "ci/bupha".into(),
    };
    assert_eq!(s.context(), "ci/bupha");
    s.post("0123456789abcdef", "pending", "ci running");
}
