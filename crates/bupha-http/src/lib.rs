//! Remote execution over HTTP — `HttpRunner` ships a tarred worktree
//! to a `bupha agent` and polls the remote job. Wire surface:
//!
//! * `POST /jobs` — `{"key","sha","cmd","env_names":[..]}` -> `{"id"}`.
//! * `PUT /jobs/{id}/worktree` — raw tar bytes; the agent extracts
//!   under its own worktree.base and starts the run.
//! * `GET /jobs/{id}` — `{"status","desc"?}` polled by `try_wait`.
//! * `DELETE /jobs/{id}` — remote kill.
//!
//! Every request is authenticated by `X-Bupha-Auth: sha256=<hex>` —
//! an HMAC-SHA256 of the request body (same scheme as GitHub webhook
//! signatures). Config carries the secret env NAME, never the value.
//!
//! Caveats mirror bupha-ssh: a killed remote job can orphan the
//! agent's grandchildren (kill is best-effort), and tarred worktrees
//! carry broken `.git` pointer files — remote runners must tolerate
//! a non-functional .git.

use bupha_runner::{RunCtx, RunHandle, Runner};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Impl-owned fields deserialized from the flattened `runner.extra`.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct HttpCfg {
    /// Agent base URL, e.g. `http://runner-box:29174`.
    pub agent_url: String,
    /// Env var NAME holding the shared HMAC secret (resolved via the
    /// usual env -> keyring -> env-file chain by the caller).
    #[serde(default = "default_secret_env")]
    pub secret_env: String,
    /// Informational pipeline name sent to the agent (its own runner
    /// config owns the actual command).
    #[serde(default)]
    pub cmd: String,
}

fn default_secret_env() -> String {
    "BUPHA_AGENT_SECRET".into()
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).expect("hmac accepts any key");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Runner impl: run jobs on a remote `bupha agent`.
pub struct HttpRunner {
    url: String,
    secret: String,
    cmd: String,
}

impl HttpRunner {
    pub fn new(cfg: HttpCfg, secret: String) -> Self {
        Self {
            url: cfg.agent_url.trim_end_matches('/').to_string(),
            secret,
            cmd: cfg.cmd,
        }
    }
}

#[derive(Serialize)]
struct JobSpec<'a> {
    key: &'a str,
    sha: &'a str,
    cmd: &'a str,
    env_names: Vec<&'a str>,
}

#[derive(Deserialize)]
struct JobCreated {
    id: String,
}

#[derive(Deserialize)]
struct JobStatus {
    status: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    code: Option<i32>,
}

fn ureq_err(prefix: &str, e: ureq::Error) -> io::Error {
    io::Error::other(format!("{prefix}: {e}"))
}

fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code << 8)
}

impl Runner for HttpRunner {
    fn name(&self) -> &'static str {
        "http"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>> {
        // Stage 1: create the remote job.
        let spec = serde_json::to_string(&JobSpec {
            key: ctx.key,
            sha: ctx.sha,
            cmd: &self.cmd,
            env_names: vec![ctx.token_env],
        })
        .expect("spec serializes");
        let resp = ureq::post(format!("{}/jobs", self.url))
            .header("X-Bupha-Auth", sign(&self.secret, spec.as_bytes()))
            .header("content-type", "application/json")
            .send(spec.as_bytes())
            .map_err(|e| ureq_err("ci infra: agent create", e))?;
        let created: JobCreated = resp
            .into_body()
            .read_json()
            .map_err(|e| ureq_err("ci infra: agent response", e))?;
        // Stage 2: tar the worktree and ship it.
        let tar = std::process::Command::new("tar")
            .args(["-cf", "-", "-C"])
            .arg(ctx.workdir)
            .arg(".")
            .output()
            .map_err(|e| io::Error::other(format!("ci infra: tar: {e}")))?;
        if !tar.status.success() {
            return Err(io::Error::other("ci infra: tar failed"));
        }
        ureq::put(format!("{}/jobs/{}/worktree", self.url, created.id))
            .header("X-Bupha-Auth", sign(&self.secret, &tar.stdout))
            .send(&tar.stdout[..])
            .map_err(|e| ureq_err("ci infra: worktree upload", e))?;
        Ok(Box::new(HttpHandle {
            url: self.url.clone(),
            id: created.id,
            secret: self.secret.clone(),
            desc: std::sync::Mutex::new(None),
        }))
    }
}

struct HttpHandle {
    url: String,
    id: String,
    secret: String,
    desc: std::sync::Mutex<Option<String>>,
}

impl RunHandle for HttpHandle {
    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        let st: JobStatus = ureq::get(format!("{}/jobs/{}", self.url, self.id))
            .header("X-Bupha-Auth", sign(&self.secret, b""))
            .call()
            .map_err(|e| ureq_err("poll", e))?
            .into_body()
            .read_json()
            .map_err(|e| ureq_err("poll parse", e))?;
        if let Some(d) = st.desc {
            *self.desc.lock().unwrap() = Some(d);
        }
        match st.status.as_str() {
            "running" => Ok(None),
            "success" => Ok(Some(exit_status(0))),
            // "failure" | "killed" | unknown -> non-zero exit.
            _ => Ok(Some(exit_status(st.code.unwrap_or(1).max(1)))),
        }
    }

    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        loop {
            if let Some(s) = self.try_wait()? {
                return Ok(s);
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        ureq::delete(format!("{}/jobs/{}", self.url, self.id))
            .header("X-Bupha-Auth", sign(&self.secret, b""))
            .call()
            .map_err(|e| ureq_err("kill", e))?;
        Ok(())
    }

    fn failure_desc(&mut self) -> Option<String> {
        self.desc.lock().unwrap().clone()
    }
}

/// The agent side — `bupha agent` serves the wire surface above and
/// runs each accepted job with the local runner it was configured
/// with. `serve` blocks until `stop` reports true (the e2e hook);
/// returns when it does.
pub mod agent {
    use super::*;

    /// One accepted remote run — the child handle plus a settled
    /// state so `kill` and post-mortem polls stay honest.
    struct RemoteJob {
        key: String,
        sha: String,
        handle: Mutex<Option<Box<dyn RunHandle>>>,
        status: Mutex<String>,
        desc: Mutex<Option<String>>,
    }

    fn verify(secret: &str, headers: &[(String, String)], body: &[u8]) -> bool {
        let Some(sig) = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-bupha-auth"))
            .map(|(_, v)| v.as_str())
        else {
            return false;
        };
        let Some(hex_sig) = sig.strip_prefix("sha256=") else {
            return false;
        };
        let Ok(raw) = hex::decode(hex_sig) else {
            return false;
        };
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes())
            .expect("hmac accepts any key length");
        mac.update(body);
        mac.verify_slice(&raw).is_ok()
    }

    fn json(status: u16, body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
        tiny_http::Response::from_string(body)
            .with_status_code(status)
            .with_header("content-type: application/json".parse::<tiny_http::Header>().unwrap())
    }

    /// Serve the agent listener. `runner` runs each accepted job;
    /// `stop` is polled between requests and while idle.
    pub fn serve(
        listen: &str,
        secret: &str,
        base: &Path,
        runner: Arc<dyn Runner>,
        stop: impl Fn() -> bool,
    ) -> io::Result<()> {
        let srv = tiny_http::Server::http(listen)
            .map_err(|e| io::Error::other(format!("agent listen {listen}: {e}")))?;
        let jobs: Arc<Mutex<std::collections::HashMap<String, Arc<RemoteJob>>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let counter = std::sync::atomic::AtomicUsize::new(0);
        loop {
            if stop() {
                return Ok(());
            }
            let Some(mut req) = srv
                .try_recv()
                .map_err(|e| io::Error::other(format!("agent recv: {e}")))?
            else {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            };
            let mut body = Vec::new();
            let _ = req
                .as_reader()
                .take(512 * 1024 * 1024)
                .read_to_end(&mut body);
            let headers: Vec<(String, String)> = req
                .headers()
                .iter()
                .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
                .collect();
            if !verify(secret, &headers, &body) {
                let _ = req.respond(json(401, r#"{"error":"bad signature"}"#));
                continue;
            }
            let method = req.method().as_str().to_string();
            let url = req.url().to_string();
            let mut parts = url.trim_matches('/').split('/');
            match (method.as_str(), parts.next(), parts.next(), parts.next()) {
                ("POST", Some("jobs"), None, None) => {
                    #[derive(Deserialize)]
                    struct Spec {
                        key: String,
                        sha: String,
                        #[serde(default)]
                        cmd: String,
                        #[serde(default)]
                        env_names: Vec<String>,
                    }
                    let Ok(spec) = serde_json::from_slice::<Spec>(&body) else {
                        let _ = req.respond(json(400, r#"{"error":"bad spec"}"#));
                        continue;
                    };
                    let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let id = format!("{}-{}-{}", spec.key.replace('/', "-"), &spec.sha[..spec.sha.len().min(12)], n);
                    let job = Arc::new(RemoteJob {
                        key: spec.key.clone(),
                        sha: spec.sha.clone(),
                        handle: Mutex::new(None),
                        status: Mutex::new(format!("pending:{}", spec.cmd)),
                        desc: Mutex::new(if spec.env_names.is_empty() {
                            None
                        } else {
                            Some(spec.env_names.join(","))
                        }),
                    });
                    jobs.lock().unwrap().insert(id.clone(), job);
                    let _ = req.respond(json(201, &format!(r#"{{"id":"{id}"}}"#)));
                }
                ("PUT", Some("jobs"), Some(id), Some("worktree")) => {
                    let dir = PathBuf::from(base).join(format!("agent-{id}"));
                    let found = jobs.lock().unwrap().get(id).cloned();
                    let Some(job) = found else {
                        let _ = req.respond(json(404, r#"{"error":"unknown job"}"#));
                        continue;
                    };
                    let status = (|| -> io::Result<()> {
                        std::fs::create_dir_all(&dir)?;
                        let mut tar = std::process::Command::new("tar")
                            .args(["-xf", "-", "-C"])
                            .arg(&dir)
                            .stdin(std::process::Stdio::piped())
                            .spawn()?;
                        tar.stdin
                            .as_mut()
                            .map(|w| w.write_all(&body).and_then(|_| w.flush()));
                        drop(tar.stdin.take());
                        if !tar.wait()?.success() {
                            return Err(io::Error::other("tar extract failed"));
                        }
                        let env_names = job
                            .desc
                            .lock()
                            .unwrap()
                            .take()
                            .unwrap_or_default();
                        let ctx = RunCtx {
                            workdir: &dir,
                            token_env: env_names.split(',').next().unwrap_or(""),
                            key: &job.key,
                            sha: &job.sha,
                        };
                        let h = runner.spawn(&ctx)?;
                        *job.handle.lock().unwrap() = Some(h);
                        *job.status.lock().unwrap() = "running".into();
                        Ok(())
                    })();
                    match status {
                        Ok(()) => {
                            let _ = req.respond(json(200, r#"{"status":"running"}"#));
                        }
                        Err(e) => {
                            *job.status.lock().unwrap() = "failure".into();
                            *job.desc.lock().unwrap() = Some("ci infra: spawn failed".into());
                            let _ = req.respond(json(500, &format!(r#"{{"error":"{e}"}}"#)));
                        }
                    }
                }
                ("GET", Some("jobs"), Some(id), None) => {
                    let found = jobs.lock().unwrap().get(id).cloned();
                    let Some(job) = found else {
                        let _ = req.respond(json(404, r#"{"error":"unknown job"}"#));
                        continue;
                    };
                    // Reap a settled child into a stable status.
                    {
                        let mut h = job.handle.lock().unwrap();
                        if let Some(hh) = h.as_mut() {
                            if *job.status.lock().unwrap() == "running" {
                                match hh.try_wait() {
                                    Ok(Some(s)) if s.success() => {
                                        *job.status.lock().unwrap() = "success".into();
                                    }
                                    Ok(Some(s)) => {
                                        *job.status.lock().unwrap() = "failure".into();
                                        if let Some(d) = hh.failure_desc() {
                                            *job.desc.lock().unwrap() = Some(d);
                                        }
                                        let _ = s;
                                    }
                                    Ok(None) => {}
                                    Err(e) => {
                                        *job.status.lock().unwrap() = "failure".into();
                                        *job.desc.lock().unwrap() =
                                            Some(format!("ci infra: {e}"));
                                    }
                                }
                            }
                        }
                    }
                    let resp = format!(
                        r#"{{"status":"{}","desc":{}}}"#,
                        job.status.lock().unwrap(),
                        serde_json::to_string(&*job.desc.lock().unwrap()).unwrap_or("null".into())
                    );
                    let _ = req.respond(json(200, &resp));
                }
                ("DELETE", Some("jobs"), Some(id), None) => {
                    let found = jobs.lock().unwrap().get(id).cloned();
                    let Some(job) = found else {
                        let _ = req.respond(json(404, r#"{"error":"unknown job"}"#));
                        continue;
                    };
                    if let Some(h) = job.handle.lock().unwrap().as_mut() {
                        let _ = h.kill();
                    }
                    *job.status.lock().unwrap() = "killed".into();
                    *job.desc.lock().unwrap() = Some("killed remotely".into());
                    let _ = req.respond(json(200, r#"{"status":"killed"}"#));
                }
                _ => {
                    let _ = req.respond(json(404, r#"{"error":"not found"}"#));
                }
            }
        }
    }
}
