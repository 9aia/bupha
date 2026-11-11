//! bupha-webhook — the forge-webhook `Source` impl: a tiny_http
//! listener that drives a `Trigger` port impl. One accept loop,
//! one request thread per delivery: route `/hook`, read the body
//! (≤ 2 MiB), repo-hint → secret select → signature verify →
//! delivery-id dedup → parse → emit the push. New forges plug in
//! as `Trigger` impls — this harness stays put.
//!
//! **Standalone/dev mode** (ADR-0007): this listener harness is the
//! *fallback* webhook ingress for deployments with no peregrin.
//! Production ingress is a peregrin campaign listener that reuses
//! bupha's normalization semantics (`Trigger` parse + HMAC verify —
//! which stay canonical here) and enqueues `ci-job` via
//! `ci.enqueue`; do not point production webhooks at `/hook`.

use bupha_trigger::{Emit, PushOutcome, Source, Stop, Trigger, Verdict};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// In-flight request-thread cap — the per-request thread exists so a
/// stalled client can't block accept, but an unbounded pool is its
/// own exhaustion vector (one socket per thread). Past the cap new
/// requests get a fast 503.
const MAX_INFLIGHT: usize = 64;

/// Decrements the in-flight counter when the request thread exits.
fn scopeguard(counter: Arc<AtomicUsize>) -> impl Drop {
    struct G(Arc<AtomicUsize>);
    impl Drop for G {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    G(counter)
}

/// Per-request state the request threads share — all read-only
/// except `seen` (the replay-dedup ring).
struct Inner {
    trigger: Arc<dyn Trigger>,
    /// Global webhook secret — repos without an override verify
    /// with it.
    secret: String,
    /// repo name → resolved secret override.
    secrets: HashMap<String, String>,
    /// Watched repo names — a repo_hint outside the set 404s.
    repos: HashSet<String>,
    /// Delivery-id dedup (bounded ring) — a replayed signed payload
    /// reuses the id and is acked without re-queueing.
    seen: Mutex<bupha_core::Seen>,
}

/// A forge-webhook ingest source — `name` identifies the configured
/// trigger ("github-webhook"). Constructed per `trigger.kind` by
/// the composition root; the forge-specific parts all live in the
/// `Trigger` impl.
pub struct WebhookSource {
    /// Human/source name for logs ("github-webhook").
    pub name: String,
    /// Listener bind address ("127.0.0.1:29173").
    pub listen: String,
    /// The forge's payload/signature impl (github today).
    pub trigger: Arc<dyn Trigger>,
    /// Global webhook secret — fallback for repos without an
    /// entry in `secrets`.
    pub secret: String,
    /// Per-repo secret overrides — repo name → resolved value.
    pub secrets: HashMap<String, String>,
    /// Watched repo names — requests for anything else 404 (an
    /// unwatched repo can never verify: no secret of ours signed it).
    pub repos: HashSet<String>,
}

impl Source for WebhookSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(&self, emit: Emit, stop: Stop) {
        let server =
            Arc::new(tiny_http::Server::http(&self.listen).expect("failed to bind listener"));
        println!("bupha watch listening on {}", self.listen);
        let inner = Arc::new(Inner {
            trigger: Arc::clone(&self.trigger),
            secret: self.secret.clone(),
            secrets: self.secrets.clone(),
            repos: self.repos.clone(),
            seen: Mutex::new(bupha_core::Seen::default()),
        });
        let inflight = Arc::new(AtomicUsize::new(0));

        // e2e hook: `stop` true → unblock the accept loop so spawned
        // test binaries shut down gracefully (no signal-handling
        // deps needed).
        {
            let srv = Arc::clone(&server);
            std::thread::spawn(move || {
                while !stop() {
                    std::thread::sleep(Duration::from_millis(50));
                }
                srv.unblock();
            });
        }

        for req in server.incoming_requests() {
            // Per-request thread — a stalled client must not block
            // the accept loop (slowloris). Body cap still applies
            // inside.
            let inner = Arc::clone(&inner);
            let emit = emit.clone();
            let inflight = Arc::clone(&inflight);
            // Admission control before spawning the thread — a busy
            // listener sheds load instead of growing unboundedly.
            if inflight.fetch_add(1, Ordering::SeqCst) >= MAX_INFLIGHT {
                inflight.fetch_sub(1, Ordering::SeqCst);
                let _ = req.respond(tiny_http::Response::from_string("busy").with_status_code(503));
                continue;
            }
            std::thread::spawn(move || {
                let _guard = scopeguard(inflight);
                let mut req = req;
                let headers: Vec<(String, String)> = req
                    .headers()
                    .iter()
                    .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
                    .collect();
                // Only the hook path exists — everything else 404s.
                if req.url().split('?').next() != Some("/hook") {
                    let _ = req.respond(
                        tiny_http::Response::from_string("not found").with_status_code(404),
                    );
                    return;
                }
                // Deliveries are POSTs — reject anything else before
                // touching the body.
                if req.method() != &tiny_http::Method::Post {
                    let _ = req.respond(
                        tiny_http::Response::from_string("method not allowed")
                            .with_status_code(405),
                    );
                    return;
                }

                let mut body = Vec::new();
                let _ = req.as_reader().take(2 * 1024 * 1024).read_to_end(&mut body);

                let trigger = &*inner.trigger;
                let (code, msg) = if trigger.is_ping(&headers) {
                    (200, "pong")
                } else if !trigger.is_event(&headers) {
                    (400, "not a CI event")
                } else {
                    // repo_hint is untrusted until verify passes — it
                    // only picks which watched repo (and secret)
                    // applies.
                    let repo = trigger
                        .repo_hint(&body)
                        .filter(|n| inner.repos.contains(n.as_str()));
                    match repo {
                        // An unwatched repo can't verify — no secret
                        // of ours was used to sign it. Reject, not
                        // ignore.
                        None => (404, "unwatched repo"),
                        Some(r)
                            if !trigger.verify(
                                inner.secrets.get(&r).unwrap_or(&inner.secret),
                                &body,
                                &headers,
                            ) =>
                        {
                            (401, "bad signature")
                        }
                        Some(_)
                            if trigger
                                .delivery_id(&headers)
                                .map(|id| !inner.seen.lock().unwrap().insert(id))
                                .unwrap_or(false) =>
                        {
                            // replayed signed payload: same delivery id
                            (200, "duplicate delivery")
                        }
                        Some(_) => match trigger.parse_push(&body, &headers) {
                            Some(PushOutcome::Skip) => (200, "ignored: no-op event"),
                            Some(PushOutcome::Push(p)) => match emit(&p) {
                                Verdict::NoMatch => (200, "ignored: not the watched branch"),
                                Verdict::Rejected => (400, "rejected: unsafe push"),
                                Verdict::Denied => (403, "sender not allowed"),
                                Verdict::Queued => (202, "queued"),
                                Verdict::Duplicate => (200, "already queued"),
                            },
                            None => (400, "unparseable payload"),
                        },
                    }
                };
                let _ = req.respond(tiny_http::Response::from_string(msg).with_status_code(code));
            });
        }
    }
}
