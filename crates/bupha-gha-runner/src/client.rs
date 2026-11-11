//! The HTTP client: one method per call the runner makes, in the
//! order a runner makes them. Blocking `ureq`, no async.
//!
//! Two auth dialects appear: `RemoteAuth <token>` while trading the
//! short-lived registration token for a tenant, `Bearer <token>`
//! everywhere after — plus the job-scoped `Bearer` the job's own
//! endpoints carry for results/run-service calls.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;

use crate::crypto;
use crate::types::*;
use crate::{Credentials, Error};

/// How long any single call may take. The message poll is a
/// long-poll — the service holds it open — so the ceiling must
/// comfortably exceed the server's own hold time.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);

#[derive(Clone)]
enum Auth {
    None,
    Bearer(String),
    /// The registration-token dialect used only for the tenant call.
    Remote(String),
}

pub struct Client {
    http: ureq::Agent,
    auth: Auth,
}

impl Client {
    pub fn new() -> Result<Self, Error> {
        let http = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // Statuses are inspected by hand: error bodies carry the
            // service's own explanation and 409s are routine.
            .http_status_as_error(false)
            .build()
            .new_agent();
        Ok(Self {
            http,
            auth: Auth::None,
        })
    }

    pub fn bearer(&mut self, token: &str) {
        self.auth = Auth::Bearer(token.to_string());
    }

    /// Bearer for job-scoped calls made on a dedicated client (the
    /// lease renewer authenticates with the job's own token).
    pub fn set_bearer(&mut self, token: &str) {
        self.auth = Auth::Bearer(token.to_string());
    }

    /// `application/x-www-form-urlencoded` POST → response text.
    /// Used only by the token endpoint.
    pub fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<String, Error> {
        let resp = self
            .http
            .post(url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send_form(fields.iter().copied())
            .map_err(Error::from)?;
        let (status, text) = Self::read(resp)?;
        Self::ok(status, &text)
    }

    /// JSON POST with an explicit Authorization header value —
    /// `None` falls back to the client's own auth. Returns nothing;
    /// for fire-and-forget calls like `renewjob`.
    pub fn post_json_raw(&self, url: &str, body: &impl Serialize) -> Result<(), Error> {
        let data = serde_json::to_vec(body).map_err(|e| Error::Protocol(e.to_string()))?;
        let _ = self.send_raw("POST", url, None, &data, "application/json")?;
        Ok(())
    }

    // --------------------------------------------------------------
    // low-level
    // --------------------------------------------------------------

    fn with_auth(
        &self,
        b: ureq::RequestBuilder<ureq::typestate::WithBody>,
    ) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let b = b.header("Accept", "application/json");
        match &self.auth {
            Auth::Bearer(t) => b.header("Authorization", &format!("Bearer {t}")),
            Auth::Remote(t) => b.header("Authorization", &format!("RemoteAuth {t}")),
            Auth::None => b,
        }
    }

    fn with_auth_simple(
        &self,
        b: ureq::RequestBuilder<ureq::typestate::WithoutBody>,
    ) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let b = b.header("Accept", "application/json");
        match &self.auth {
            Auth::Bearer(t) => b.header("Authorization", &format!("Bearer {t}")),
            Auth::Remote(t) => b.header("Authorization", &format!("RemoteAuth {t}")),
            Auth::None => b,
        }
    }

    fn read(resp: ureq::http::Response<ureq::Body>) -> Result<(u16, String), Error> {
        let status = resp.status().as_u16();
        let text = resp
            .into_body()
            .read_to_string()
            .map_err(|e| Error::Http(format!("read body: {e}")))?;
        Ok((status, text))
    }

    fn ok(status: u16, text: &str) -> Result<String, Error> {
        if !(200..300).contains(&status) {
            return Err(Error::Status {
                status,
                body: crate::shown(text),
            });
        }
        Ok(text.to_string())
    }

    /// JSON call with a request body (POST/PUT/PATCH).
    fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        url: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, Error> {
        let req = match method {
            "POST" => self.with_auth(self.http.post(url)),
            "PUT" => self.with_auth(self.http.put(url)),
            "PATCH" => self.with_auth(self.http.patch(url)),
            other => return Err(Error::Protocol(format!("bad method {other}"))),
        };
        let resp = match body {
            Some(b) => req.send_json(b).map_err(Error::from)?,
            None => req.send(&[] as &[u8]).map_err(Error::from)?,
        };
        let (status, text) = Self::read(resp)?;
        let text = Self::ok(status, &text)?;
        if text.trim().is_empty() {
            return Err(Error::Protocol(format!("empty response from {url}")));
        }
        serde_json::from_str(&text).map_err(|e| {
            Error::Protocol(format!(
                "bad JSON from {url}: {e} — {}",
                crate::shown(&text)
            ))
        })
    }

    /// Body-less JSON call (GET/DELETE).
    fn call_simple<T: DeserializeOwned>(&self, method: &str, url: &str) -> Result<T, Error> {
        let req = match method {
            "GET" => self.with_auth_simple(self.http.get(url)),
            "DELETE" => self.with_auth_simple(self.http.delete(url)),
            other => return Err(Error::Protocol(format!("bad method {other}"))),
        };
        let resp = req.call().map_err(Error::from)?;
        let (status, text) = Self::read(resp)?;
        let text = Self::ok(status, &text)?;
        if text.trim().is_empty() {
            return Err(Error::Protocol(format!("empty response from {url}")));
        }
        serde_json::from_str(&text).map_err(|e| {
            Error::Protocol(format!(
                "bad JSON from {url}: {e} — {}",
                crate::shown(&text)
            ))
        })
    }

    /// Long-poll; an empty 2xx body means "nothing yet".
    fn maybe<T: DeserializeOwned>(&self, url: &str) -> Result<Option<T>, Error> {
        let resp = self
            .with_auth_simple(self.http.get(url))
            .call()
            .map_err(Error::from)?;
        let (status, text) = Self::read(resp)?;
        let text = Self::ok(status, &text)?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| Error::Protocol(format!("bad JSON: {e} — {}", crate::shown(&text))))
    }

    fn nothing(&self, method: &str, url: &str) -> Result<(), Error> {
        match method {
            "GET" | "DELETE" => {
                let req = if method == "GET" {
                    self.with_auth_simple(self.http.get(url))
                } else {
                    self.with_auth_simple(self.http.delete(url))
                };
                let resp = req.call().map_err(Error::from)?;
                let (status, text) = Self::read(resp)?;
                Self::ok(status, &text)?;
            }
            _ => {
                let req = match method {
                    "POST" => self.with_auth(self.http.post(url)),
                    "PUT" => self.with_auth(self.http.put(url)),
                    "PATCH" => self.with_auth(self.http.patch(url)),
                    other => return Err(Error::Protocol(format!("bad method {other}"))),
                };
                let resp = req.send(&[] as &[u8]).map_err(Error::from)?;
                let (status, text) = Self::read(resp)?;
                Self::ok(status, &text)?;
            }
        }
        Ok(())
    }

    fn send_raw(
        &self,
        method: &str,
        url: &str,
        auth: Option<&str>,
        body: &[u8],
        content_type: &str,
    ) -> Result<String, Error> {
        let req = match method {
            "POST" => self.http.post(url),
            "PUT" => self.http.put(url),
            _ => return Err(Error::Protocol(format!("bad method {method}"))),
        }
        .header("Accept", "application/json")
        .header("Content-Type", content_type);
        let req = match auth {
            Some(a) => req.header("Authorization", a),
            None => match &self.auth {
                Auth::Bearer(t) => req.header("Authorization", &format!("Bearer {t}")),
                Auth::Remote(t) => req.header("Authorization", &format!("RemoteAuth {t}")),
                Auth::None => req,
            },
        };
        let resp = req.send(body).map_err(Error::from)?;
        Self::body_text(resp)
    }

    /// Status-checked response body — what `send_raw` hands back.
    fn body_text(resp: ureq::http::Response<ureq::Body>) -> Result<String, Error> {
        let (status, text) = Self::read(resp)?;
        Self::ok(status, &text)
    }

    fn send_json_as<T: DeserializeOwned>(
        &self,
        method: &str,
        url: &str,
        auth: Option<&str>,
        body: &impl Serialize,
    ) -> Result<T, Error> {
        let text = self.send_raw_json(method, url, auth, body)?;
        serde_json::from_str(&text).map_err(|e| {
            Error::Protocol(format!(
                "bad JSON from {url}: {e} — {}",
                crate::shown(&text)
            ))
        })
    }

    fn send_raw_json(
        &self,
        method: &str,
        url: &str,
        auth: Option<&str>,
        body: &impl Serialize,
    ) -> Result<String, Error> {
        let data = serde_json::to_vec(body).map_err(|e| Error::Protocol(e.to_string()))?;
        self.send_raw(method, url, auth, &data, "application/json")
    }

    // --------------------------------------------------------------
    // registration
    // --------------------------------------------------------------

    /// Trade the registration token for the Actions service URL and
    /// a bearer token for it.
    pub fn tenant(&mut self, repo_url: &str, registration_token: &str) -> Result<Tenant, Error> {
        self.auth = Auth::Remote(registration_token.to_string());
        let url = registration_endpoint(repo_url);
        self.call(
            "POST",
            &url,
            Some(&TenantRequest {
                url: repo_url,
                runner_event: "register",
            }),
        )
    }

    /// `poolName=Default` → the pool id a repository runner joins.
    pub fn default_pool(&self, service: &str) -> Result<i64, Error> {
        let url = format!(
            "{}/_apis/distributedtask/pools?poolName=Default&api-version=6.0-preview.1",
            service.trim_end_matches('/')
        );
        let pools: Pools = self.call_simple("GET", &url)?;
        pools
            .value
            .first()
            .map(|p| p.id)
            .ok_or_else(|| Error::Protocol("there is no Default pool to join".into()))
    }

    pub fn create_agent(
        &self,
        service: &str,
        pool: i64,
        agent: &AgentRequest,
    ) -> Result<Agent, Error> {
        let url = format!(
            "{}/_apis/distributedtask/pools/{pool}/agents?api-version=6.0-preview.2",
            service.trim_end_matches('/')
        );
        self.call("POST", &url, Some(agent))
    }

    /// Mint a short-lived runner registration token with a PAT —
    /// the modern equivalent of the token the settings UI shows.
    pub fn registration_token(
        &self,
        api_base: &str,
        pat: &str,
        repo: &str,
    ) -> Result<String, Error> {
        #[derive(serde::Deserialize)]
        struct Minted {
            token: String,
        }
        let url = format!(
            "{}/repos/{repo}/actions/runners/registration-token",
            api_base.trim_end_matches('/')
        );
        let b = self
            .http
            .post(&url)
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", &format!("Bearer {pat}"))
            .header("User-Agent", "bupha-gha-runner");
        let resp = b.send(&[] as &[u8]).map_err(Error::from)?;
        let status = resp.status().as_u16();
        let text = resp
            .into_body()
            .read_to_string()
            .map_err(|e| Error::Http(format!("read body: {e}")))?;
        if !(200..300).contains(&status) {
            return Err(Error::Status {
                status,
                body: crate::shown(&text),
            });
        }
        let minted: Minted = serde_json::from_str(&text)
            .map_err(|e| Error::Protocol(format!("bad mint response: {e}")))?;
        Ok(minted.token)
    }

    // --------------------------------------------------------------
    // session + queue
    // --------------------------------------------------------------

    pub fn create_session(&self, credentials: &Credentials, name: &str) -> Result<Session, Error> {
        let url = format!(
            "{}/_apis/distributedtask/pools/{}/sessions?api-version=5.1-preview",
            credentials.service_url.trim_end_matches('/'),
            credentials.pool_id,
        );
        self.call(
            "POST",
            &url,
            Some(&SessionRequest {
                agent: AgentReference {
                    id: credentials.agent_id,
                    name,
                },
                owner_name: name,
            }),
        )
    }

    pub fn delete_session(&self, credentials: &Credentials, session: &str) -> Result<(), Error> {
        let url = format!(
            "{}/_apis/distributedtask/pools/{}/sessions/{session}?api-version=5.1-preview",
            credentials.service_url.trim_end_matches('/'),
            credentials.pool_id,
        );
        self.nothing("DELETE", &url)
    }

    /// Long-poll; an empty body means "nothing yet".
    pub fn messages(
        &self,
        credentials: &Credentials,
        session: &str,
        last: Option<i64>,
    ) -> Result<Option<Envelope>, Error> {
        let mut url = format!(
            "{}/_apis/distributedtask/pools/{}/messages?api-version=5.1-preview&sessionId={session}&runnerVersion={RUNNER_VERSION}",
            credentials.service_url.trim_end_matches('/'),
            credentials.pool_id,
        );
        if let Some(last) = last {
            url.push_str(&format!("&lastMessageId={last}"));
        }
        self.maybe(&url)
    }

    /// Acknowledge a queue message once its job is done — a crash
    /// before this leaves the job to be handed out again.
    pub fn acknowledge(
        &self,
        credentials: &Credentials,
        session: &str,
        message: i64,
    ) -> Result<(), Error> {
        let url = format!(
            "{}/_apis/distributedtask/pools/{}/messages/{message}?api-version=5.1-preview&sessionId={session}",
            credentials.service_url.trim_end_matches('/'),
            credentials.pool_id,
        );
        self.nothing("DELETE", &url)
    }

    // --------------------------------------------------------------
    // broker (post-migration flow)
    // --------------------------------------------------------------

    /// The broker keeps its own session; the agent's *public* key goes
    /// along so the session key can be wrapped to it.
    pub fn create_broker_session(
        &self,
        broker: &str,
        credentials: &Credentials,
        name: &str,
    ) -> Result<Session, Error> {
        let key = crypto::read_key(&credentials.private_key)?;
        let url = format!("{}/session", broker.trim_end_matches('/'));
        self.call(
            "POST",
            &url,
            Some(&BrokerSessionRequest {
                agent: BrokerAgent {
                    id: credentials.agent_id,
                    name,
                    version: RUNNER_VERSION,
                    authorization: PublicKeyRequest {
                        public_key: crypto::public_key(&key),
                    },
                },
                owner_name: name,
                use_fips_encryption: false,
            }),
        )
    }

    pub fn broker_message(
        &self,
        broker: &str,
        session: &str,
        os: &str,
        arch: &str,
    ) -> Result<Option<Envelope>, Error> {
        let url = format!(
            "{}/message?sessionId={session}&status=Online&runnerVersion={RUNNER_VERSION}&os={os}&architecture={arch}&disableUpdate=true",
            broker.trim_end_matches('/'),
        );
        self.maybe(&url)
    }

    // --------------------------------------------------------------
    // job lifecycle on the run service
    // --------------------------------------------------------------

    /// The broker sends where to collect the job from; this fetches
    /// the expanded `JobMessage`.
    pub fn acquire_job(&self, offer: &JobOffer) -> Result<JobMessage, Error> {
        let url = format!("{}/acquirejob", offer.run_service_url.trim_end_matches('/'));
        let text = self.send_raw_json(
            "POST",
            &url,
            None,
            &AcquireJobRequest {
                job_message_id: &offer.runner_request_id,
                runner_os: crate::host_os(),
                billing_owner_id: &offer.billing_owner_id,
            },
        )?;
        JobMessage::decode(&text).map_err(Error::Protocol)
    }

    /// Renew the lease while a job runs — the service hands the job
    /// to somebody else once the lease lapses.
    pub fn renew_job(&self, job: &JobMessage) -> Result<(), Error> {
        let Some(base) = job.run_service_url() else {
            return Ok(());
        };
        let url = format!("{}/renewjob", base.trim_end_matches('/'));
        let auth = job_auth(job, &self.auth_header());
        self.send_raw_json(
            "POST",
            &url,
            auth.as_deref(),
            &RenewJobRequest {
                plan_id: &job.plan_id,
                job_id: &job.job_id,
            },
        )?;
        Ok(())
    }

    /// Report what became of the job. Until this lands the service
    /// still counts the runner as busy. `outputs` carries the
    /// evaluated job `outputs:` map — the run service stores it so
    /// `needs.<job>.outputs.*` resolves on dependent jobs.
    pub fn complete_job(
        &self,
        offer: Option<&JobOffer>,
        job: &JobMessage,
        outcome: Outcome,
        outputs: &BTreeMap<String, Variable>,
        steps: &[StepResult],
    ) -> Result<(), Error> {
        let base = offer
            .map(|o| o.run_service_url.clone())
            .or_else(|| job.run_service_url().map(str::to_string));
        let Some(base) = base else {
            return Err(Error::Protocol("job has no run service url".into()));
        };
        let url = format!("{}/completejob", base.trim_end_matches('/'));
        let billing = offer
            .map(|o| o.billing_owner_id.as_str())
            .unwrap_or(job.billing_owner_id.as_str());
        let auth = job_auth(job, &self.auth_header());
        self.send_raw_json(
            "POST",
            &url,
            auth.as_deref(),
            &CompleteJobRequest {
                plan_id: &job.plan_id,
                job_id: &job.job_id,
                conclusion: outcome.name(),
                billing_owner_id: billing,
                outputs,
                step_results: steps,
            },
        )?;
        Ok(())
    }

    // --------------------------------------------------------------
    // results service — step states and logs
    // --------------------------------------------------------------

    /// Tell the results service where the steps have got to.
    pub fn update_steps(
        &self,
        job: &JobMessage,
        results_url: &str,
        change_order: u64,
        steps: &[StepResult],
    ) -> Result<(), Error> {
        let url = format!(
            "{}/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate",
            results_url.trim_end_matches('/')
        );
        let auth = job_auth(job, &self.auth_header());
        self.send_raw_json(
            "POST",
            &url,
            auth.as_deref(),
            &StepsUpdateRequest {
                workflow_run_backend_id: &job.plan_id,
                workflow_job_run_backend_id: &job.job_id,
                change_order,
                steps,
            },
        )?;
        Ok(())
    }

    /// Upload what a step (or the whole job) printed: ask where, put
    /// it there, say it landed.
    pub fn upload_log(
        &self,
        job: &JobMessage,
        results_url: &str,
        step: Option<&str>,
        log: &str,
    ) -> Result<(), Error> {
        let (get_url_ep, meta_ep) = match step {
            Some(_) => (
                "twirp/results.services.receiver.Receiver/GetStepLogsSignedBlobURL",
                "twirp/results.services.receiver.Receiver/CreateStepLogsMetadata",
            ),
            None => (
                "twirp/results.services.receiver.Receiver/GetJobLogsSignedBlobURL",
                "twirp/results.services.receiver.Receiver/CreateJobLogsMetadata",
            ),
        };
        let base = results_url.trim_end_matches('/');
        let signed: SignedLogUrl = self.send_json_as(
            "POST",
            &format!("{base}/{get_url_ep}"),
            job_auth(job, &self.auth_header()).as_deref(),
            &SignedLogUrlRequest {
                workflow_run_backend_id: &job.plan_id,
                workflow_job_run_backend_id: &job.job_id,
                step_backend_id: step,
            },
        )?;
        self.put_append_blob(&signed, log)?;
        self.send_raw_json(
            "POST",
            &format!("{base}/{meta_ep}"),
            job_auth(job, &self.auth_header()).as_deref(),
            &LogsMetadata {
                workflow_run_backend_id: &job.plan_id,
                workflow_job_run_backend_id: &job.job_id,
                step_backend_id: step,
                uploaded_at: timestamp(std::time::SystemTime::now()),
                line_count: log.lines().count() as u64,
            },
        )?;
        Ok(())
    }

    /// Azure refuses a blob that does not say what kind it is:
    /// create the append blob, then append the log and seal it.
    fn put_append_blob(&self, signed: &SignedLogUrl, log: &str) -> Result<(), Error> {
        let create = self
            .http
            .put(&signed.logs_url)
            .header("x-ms-blob-type", "AppendBlob")
            .header("Content-Type", "text/plain")
            .header("Content-Length", "0");
        let resp = create.send(&[] as &[u8]).map_err(Error::from)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.into_body().read_to_string().unwrap_or_default();
            return Err(Error::Status {
                status,
                body: crate::shown(&text),
            });
        }
        let sep = if signed.logs_url.contains('?') {
            "&"
        } else {
            "?"
        };
        let append = format!("{}{sep}comp=appendblock&seal=true", signed.logs_url);
        let resp = self
            .http
            .put(&append)
            .header("Content-Type", "text/plain")
            .send(log.as_bytes())
            .map_err(Error::from)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.into_body().read_to_string().unwrap_or_default();
            return Err(Error::Status {
                status,
                body: crate::shown(&text),
            });
        }
        Ok(())
    }

    fn auth_header(&self) -> Option<String> {
        match &self.auth {
            Auth::Bearer(t) => Some(format!("Bearer {t}")),
            Auth::Remote(t) => Some(format!("RemoteAuth {t}")),
            Auth::None => None,
        }
    }
}

/// Job-scoped calls authenticate with the token the job was issued,
/// falling back to the runner's own.
fn job_auth(job: &JobMessage, fallback: &Option<String>) -> Option<String> {
    job.service_token()
        .map(|t| format!("Bearer {t}"))
        .or_else(|| fallback.clone())
}

/// Where registration tokens are traded in — moves for GHES.
fn registration_endpoint(repo_url: &str) -> String {
    let host = repo_url
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or_default())
        .unwrap_or_default();
    match host {
        "github.com" | "www.github.com" => {
            "https://api.github.com/actions/runner-registration".to_string()
        }
        other => format!("https://{other}/api/v3/actions/runner-registration"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn github_dot_com_registers_against_the_public_api() {
        assert_eq!(
            registration_endpoint("https://github.com/octocat/hello"),
            "https://api.github.com/actions/runner-registration"
        );
    }

    #[test]
    fn enterprise_registers_against_its_own_host() {
        assert_eq!(
            registration_endpoint("https://ghe.example.com/octocat/hello"),
            "https://ghe.example.com/api/v3/actions/runner-registration"
        );
    }
}
