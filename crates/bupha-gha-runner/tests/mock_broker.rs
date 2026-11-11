//! End-to-end against a mock broker: registration credentials are
//! fabricated, a tiny_http server plays the token endpoint, the
//! message queue, the run service (`acquirejob`/`completejob`) and
//! session lifecycle — the agent must poll, acquire, execute the
//! script step and report `succeeded`.

#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bupha_gha_runner::register::Credentials;
use bupha_gha_runner::{Agent, Client, Config};

fn respond(req: tiny_http::Request, status: u16, body: &str) {
    let _ =
        req.respond(tiny_http::Response::from_string(body.to_string()).with_status_code(status));
}

fn path_of(req: &tiny_http::Request) -> String {
    req.url().split('?').next().unwrap_or("").to_string()
}

#[test]
fn agent_runs_a_job_end_to_end_against_a_mock_broker() {
    let stop = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(Mutex::new(None));
    let polls = Arc::new(AtomicUsize::new(0));

    // The server binds first so `base` is known before it answers.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let server_stop = stop.clone();
    let server_completed = completed.clone();
    let server_polls = polls.clone();
    std::thread::spawn(move || {
        let listener = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = match listener.server_addr() {
            tiny_http::ListenAddr::IP(a) => a,
            other => panic!("unexpected listen addr {other:?}"),
        };
        let base = format!("http://{addr}");
        tx.send(base.clone()).unwrap();
        let mut offer_sent = false;
        for mut req in listener.incoming_requests() {
            let mut body = String::new();
            // Only body-carrying methods have a length to read — a
            // GET's reader would wait on the connection otherwise.
            if matches!(req.method().as_str(), "POST" | "PUT" | "PATCH") {
                let _ = req.as_reader().read_to_string(&mut body);
            }
            let path = path_of(&req);
            let method = req.method().as_str().to_string();

            if path == "/token" && method == "POST" {
                respond(req, 200, r#"{"access_token":"tok-1","expires_in":3600}"#);
                continue;
            }
            if path == "/_apis/distributedtask/pools/1/sessions" && method == "POST" {
                respond(req, 200, r#"{"sessionId":"sess-1"}"#);
                continue;
            }
            if path.starts_with("/_apis/distributedtask/pools/1/messages") {
                if method == "GET" {
                    let n = server_polls.fetch_add(1, Ordering::SeqCst);
                    if !offer_sent {
                        offer_sent = true;
                        let offer = serde_json::json!({
                            "runServiceUrl": format!("{base}/run"),
                            "runnerRequestId": "req-1",
                            "billingOwnerId": "owner-1",
                            "shouldAcknowledge": true,
                        });
                        let envelope = serde_json::json!({
                            "messageId": 1,
                            "messageType": "RunnerJobRequest",
                            "body": offer.to_string(),
                        });
                        respond(req, 200, &envelope.to_string());
                    } else {
                        // Nothing queued — but don't spin forever if
                        // the agent never completes the job.
                        respond(req, 200, "");
                        if n > 10 {
                            server_stop.store(true, Ordering::SeqCst);
                        }
                    }
                    continue;
                }
                if method == "DELETE" {
                    respond(req, 200, "{}");
                    continue;
                }
            }
            if path.starts_with("/_apis/distributedtask/pools/1/sessions/") && method == "DELETE" {
                respond(req, 200, "{}");
                continue;
            }
            if path == "/run/acquirejob" && method == "POST" {
                let job = serde_json::json!({
                    "messageType": "PipelineAgentJobRequest",
                    "jobId": "job-mock-1",
                    "jobDisplayName": "mock job",
                    "jobName": "mock",
                    "requestId": 11,
                    "billingOwnerId": "owner-1",
                    "plan": {"planId": "plan-mock"},
                    "timeline": {"id": "tl-mock"},
                    "contextData": {"t": 2, "d": [
                        {"k": "github", "v": {"t": 2, "d": [
                            {"k": "repository", "v": "octocat/hello"}
                        ]}}
                    ]},
                    "variables": {},
                    "resources": {"endpoints": [{
                        "url": format!("{base}/run"),
                        "data": {}
                    }]},
                    "environmentVariables": {"t": 2, "d": []},
                    "jobOutputs": {"type": 2, "map": [
                        {"Key": {"type": 0, "lit": "answer"},
                         "Value": {"type": 3, "expr": "steps.s1.outputs.answer"}}
                    ]},
                    "steps": [{
                        "type": "action",
                        "id": "s1",
                        "name": "__run",
                        "displayNameToken": {"type": 0, "lit": "say hi"},
                        "reference": {"type": "script"},
                        "inputs": {"type": 2, "map": [
                            {"Key": {"type": 0, "lit": "script"},
                             "Value": {"type": 0, "lit": "echo mock-broker-ok\necho \"answer=42\" >> \"$GITHUB_OUTPUT\""}},
                            {"Key": {"type": 0, "lit": "shell"},
                             "Value": {"type": 0, "lit": "sh"}}
                        ]}
                    }]
                });
                respond(req, 200, &job.to_string());
                continue;
            }
            if path == "/run/completejob" && method == "POST" {
                *server_completed.lock().unwrap() = Some(body);
                server_stop.store(true, Ordering::SeqCst);
                respond(req, 200, "{}");
                continue;
            }
            respond(req, 404, "not found");
        }
    });

    let base = rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // Fabricate registered credentials — no real registration call.
    let key = bupha_gha_runner::crypto::generate_key().unwrap();
    let credentials = Credentials {
        service_url: base.clone(),
        pool_id: 1,
        agent_id: 2,
        agent_name: "bupha-mock".into(),
        client_id: "cid".into(),
        token_url: format!("{base}/token"),
        private_key: bupha_gha_runner::crypto::private_key_pem(&key).unwrap(),
    };
    let workdir = std::env::temp_dir().join(format!(
        "bupha-gha-runner-mock-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config = Config {
        workdir: workdir.clone(),
        ..Config::default()
    };

    let mut agent = Agent::connect(Client::new().unwrap(), credentials, config).unwrap();
    agent.run(&stop).unwrap();

    let done = completed.lock().unwrap().clone();
    let _ = std::fs::remove_dir_all(workdir.parent().unwrap_or(&workdir));

    let body = done.expect("completejob was never called");
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(report["conclusion"], "succeeded");
    assert_eq!(report["planId"], "plan-mock");
    assert_eq!(report["jobId"], "job-mock-1");
    assert_eq!(report["stepResults"][0]["conclusion"], "succeeded");
    // The evaluated job `outputs:` map rides completejob — this is
    // the channel `needs.*.outputs` consumers read back.
    assert_eq!(report["outputs"]["answer"]["value"], "42");
}
