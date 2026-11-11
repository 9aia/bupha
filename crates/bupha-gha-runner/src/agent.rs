//! The listener: connect, open a session, long-poll the queue, run
//! each job offered, report the outcome, acknowledge the message.
//!
//! Two queue generations are handled: the service's own message
//! queue (`/_apis/distributedtask/…/messages`) and the broker the
//! service migrates runners to (`BrokerMigration` → `/session` +
//! `/message`). Job *offers* (`RunnerJobRequest`) are collected via
//! `{run_service}/acquirejob`; inline jobs
//! (`PipelineAgentJobRequest`) decode straight from the body.

use base64::Engine as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::client::Client;
use crate::crypto;
use crate::exec::{self, JobRunner};
use crate::register::Credentials;
use crate::report::Reporter;
use crate::types::*;
use crate::{Config, Error};

/// Token refresh margin — renew when less than this remains.
const TOKEN_MARGIN: Duration = Duration::from_secs(300);
/// Session-open conflicts (a session still held by a dead run) are
/// waited out: the service lapses them.
const SESSION_RETRY: Duration = Duration::from_secs(10);
const SESSION_ATTEMPTS: u32 = 30;
/// Job lease renewal cadence.
const RENEW_EVERY: Duration = Duration::from_secs(60);

struct Token {
    value: String,
    expires_in: u64,
    issued: Instant,
}

pub struct Agent {
    client: Client,
    credentials: Credentials,
    config: Config,
    token: Token,
    session_key: Option<Vec<u8>>,
    broker: Option<String>,
    broker_session: Option<Session>,
    /// The job currently executing — cancellation messages reach it.
    current: Mutex<Option<Arc<JobRunner>>>,
}

impl Agent {
    /// Connect with already-registered credentials.
    pub fn connect(
        mut client: Client,
        credentials: Credentials,
        config: Config,
    ) -> Result<Self, Error> {
        let token = Self::fetch_token(&mut client, &credentials)?;
        client.bearer(&token.value);
        Ok(Self {
            client,
            credentials,
            config,
            token,
            session_key: None,
            broker: None,
            broker_session: None,
            current: Mutex::new(None),
        })
    }

    /// Register-or-load credentials from `config`, then connect.
    pub fn from_config(config: Config) -> Result<Self, Error> {
        let mut client = Client::new()?;
        let credentials = crate::register::ensure_credentials(&mut client, &config)?;
        Self::connect(client, credentials, config)
    }

    fn fetch_token(client: &mut Client, credentials: &Credentials) -> Result<Token, Error> {
        #[derive(serde::Deserialize)]
        struct T {
            access_token: String,
            expires_in: u64,
        }
        let assertion = crypto::sign_assertion(credentials)?;
        let resp = client.post_form(
            &credentials.token_url,
            &[
                ("grant_type", "client_credentials"),
                (
                    "client_assertion_type",
                    "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
                ),
                ("client_assertion", assertion.as_str()),
            ],
        )?;
        let t: T = serde_json::from_str(&resp)
            .map_err(|e| Error::Protocol(format!("bad token response: {e}")))?;
        Ok(Token {
            value: t.access_token,
            expires_in: t.expires_in,
            issued: Instant::now(),
        })
    }

    fn refresh_token_if_stale(&mut self) -> Result<(), Error> {
        if self.token.issued.elapsed() + TOKEN_MARGIN < Duration::from_secs(self.token.expires_in) {
            return Ok(());
        }
        self.token = Self::fetch_token(&mut self.client, &self.credentials)?;
        self.client.bearer(&self.token.value);
        Ok(())
    }

    /// The poll loop. `stop` is checked between polls and during
    /// jobs; returns Ok on a clean stop, Err on protocol/transport
    /// failure (the caller decides whether to reconnect).
    pub fn run(&mut self, stop: &AtomicBool) -> Result<(), Error> {
        let session = self.open_session()?;
        let mut last_message: Option<i64> = None;
        loop {
            if stop.load(Ordering::SeqCst) {
                let _ = self
                    .client
                    .delete_session(&self.credentials, &session.session_id);
                return Ok(());
            }
            self.refresh_token_if_stale()?;
            let Some(envelope) = self.poll(&session, last_message)? else {
                continue;
            };

            // Handed to the broker — not work; the next poll goes there.
            if let Some(broker) = envelope.broker_url() {
                eprintln!("gha-runner: handed over to broker {broker}");
                match self.client.create_broker_session(
                    &broker,
                    &self.credentials,
                    self.agent_name(),
                ) {
                    Ok(s) => {
                        self.broker_session = Some(s);
                        self.broker = Some(broker);
                    }
                    Err(e) => eprintln!("gha-runner: broker session failed: {e}"),
                }
                continue;
            }
            if let Some(id) = envelope.message_id {
                last_message = Some(id);
            }

            if envelope.is_cancel() {
                if let Some(runner) = self.current.lock().unwrap().as_ref() {
                    runner.cancel();
                }
                self.ack(&session, envelope.message_id);
                continue;
            }
            if envelope.is_update() {
                self.ack(&session, envelope.message_id);
                continue;
            }

            // Resolve the job: broker offers are pointers to collect;
            // inline jobs decode from the body.
            let offer = envelope.job_offer();
            let inline = if offer.is_none() && envelope.is_inline_job() {
                Some(envelope.body.clone())
            } else {
                None
            };
            let job = match offer {
                Some(o) => match self.client.acquire_job(&o) {
                    Ok(j) => Some((j, Some(o))),
                    Err(e) => {
                        eprintln!(
                            "gha-runner: cannot acquire job {}: {e}",
                            o.runner_request_id
                        );
                        None
                    }
                },
                None => match inline {
                    Some(body) => match self.decode_job_body(&envelope, &body) {
                        Ok(j) => Some((j, None)),
                        Err(e) => {
                            eprintln!("gha-runner: cannot decode job message: {e}");
                            None
                        }
                    },
                    None => None,
                },
            };

            if let Some((job, offer)) = job {
                self.run_job(&job, offer.as_ref(), stop)?;
            }
            self.ack(&session, envelope.message_id);
        }
    }

    fn agent_name(&self) -> &str {
        if self.credentials.agent_name.is_empty() {
            "bupha"
        } else {
            &self.credentials.agent_name
        }
    }

    fn ack(&self, session: &Session, message_id: Option<i64>) {
        if let Some(id) = message_id {
            if let Err(e) = self
                .client
                .acknowledge(&self.credentials, &session.session_id, id)
            {
                eprintln!("gha-runner: cannot ack message {id}: {e}");
            }
        }
    }

    /// Open a service session, waiting out 409 conflicts.
    fn open_session(&mut self) -> Result<Session, Error> {
        let name = self.agent_name().to_string();
        let mut attempt = 0;
        loop {
            match self.client.create_session(&self.credentials, &name) {
                Err(Error::Status { status: 409, .. }) if attempt < SESSION_ATTEMPTS => {
                    attempt += 1;
                    eprintln!("gha-runner: session conflict; retry {attempt}/{SESSION_ATTEMPTS}");
                    std::thread::sleep(SESSION_RETRY);
                }
                Err(e) => return Err(e),
                Ok(session) => {
                    self.session_key = self.unwrap_key(&session)?;
                    return Ok(session);
                }
            }
        }
    }

    /// The session's AES key — plain base64, or RSA-OAEP wrapped to
    /// the runner's private key.
    fn unwrap_key(&self, session: &Session) -> Result<Option<Vec<u8>>, Error> {
        let Some(key) = &session.encryption_key else {
            return Ok(None);
        };
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&key.value)
            .map_err(|e| Error::Crypto(format!("session key is not base64: {e}")))?;
        if !key.encrypted {
            return Ok(Some(raw));
        }
        let private = crypto::read_key(&self.credentials.private_key)?;
        Ok(Some(crypto::unwrap_session_key(
            &private,
            &raw,
            session.use_fips_encryption,
        )?))
    }

    fn poll(&mut self, session: &Session, last: Option<i64>) -> Result<Option<Envelope>, Error> {
        match &self.broker {
            Some(broker) => {
                let id = self
                    .broker_session
                    .as_ref()
                    .map(|s| s.session_id.as_str())
                    .unwrap_or(&session.session_id);
                self.client
                    .broker_message(broker, id, crate::host_os(), crate::host_arch())
            }
            None => self
                .client
                .messages(&self.credentials, &session.session_id, last),
        }
    }

    /// Decode a job from an inline message body — plain JSON, base64
    /// JSON (older queue encoding), or AES-256-CBC under the session
    /// key.
    fn decode_job_body(&self, envelope: &Envelope, body: &str) -> Result<JobMessage, Error> {
        if let Some(key) = &self.session_key {
            let iv_b64 = envelope
                .iv
                .as_deref()
                .ok_or_else(|| Error::Protocol("encrypted message with no iv".into()))?;
            let iv = base64::engine::general_purpose::STANDARD
                .decode(iv_b64)
                .map_err(|e| Error::Crypto(format!("iv is not base64: {e}")))?;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(body)
                .map_err(|e| Error::Crypto(format!("body is not base64: {e}")))?;
            let plain = crypto::decrypt_message(key, &iv, &raw)?;
            return JobMessage::decode(&plain).map_err(Error::Protocol);
        }
        if let Ok(job) = JobMessage::decode(body) {
            return Ok(job);
        }
        // Older queues base64 the body.
        let raw = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|e| Error::Protocol(format!("job body is neither JSON nor base64: {e}")))?;
        let text = String::from_utf8(raw)
            .map_err(|e| Error::Protocol(format!("job body is not utf8: {e}")))?;
        JobMessage::decode(&text).map_err(Error::Protocol)
    }

    /// Execute one job end to end: workspace, steps, reporting,
    /// completion. The lease renewal runs on a side thread.
    fn run_job(
        &mut self,
        job: &JobMessage,
        offer: Option<&JobOffer>,
        stop: &AtomicBool,
    ) -> Result<(), Error> {
        eprintln!(
            "gha-runner: running job {} ({}) — {} step(s)",
            job.job_display_name,
            job.job_id,
            job.steps.len()
        );
        let ws = self
            .config
            .workdir
            .join(format!("job-{}", safe(&job.job_id)));
        std::fs::create_dir_all(&ws)?;

        let runner = Arc::new(JobRunner::new());
        *self.current.lock().unwrap() = Some(runner.clone());

        // Lease renewal on a side thread — the run service hands the
        // job to somebody else once the lease lapses.
        let renew_stop = Arc::new(AtomicBool::new(false));
        let renew_thread = self.spawn_renewer(job, renew_stop.clone());

        let mut reporter = Reporter::new(&self.client, job, offer);
        // `stop` (the caller's shutdown flag) also cancels the job in
        // flight: a scoped watcher flips the runner's cancel flag.
        let done = Arc::new(AtomicBool::new(false));
        let outcome = std::thread::scope(|s| {
            {
                let runner = runner.clone();
                let done = done.clone();
                s.spawn(move || {
                    while !done.load(Ordering::SeqCst) {
                        if stop.load(Ordering::SeqCst) {
                            runner.cancel();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                });
            }
            let out = exec::execute(job, &ws, &runner, &mut reporter);
            // Inside the scope: the watcher exits on this flag, and
            // `scope` waits for it before returning.
            done.store(true, Ordering::SeqCst);
            out
        });
        let job_outcome = outcome.conclusion;

        renew_stop.store(true, Ordering::SeqCst);
        if let Some(t) = renew_thread {
            let _ = t.join();
        }
        *self.current.lock().unwrap() = None;

        reporter.finish(&outcome);
        eprintln!(
            "gha-runner: job {} finished: {}",
            job.job_id,
            job_outcome.name()
        );
        Ok(())
    }

    /// Renew the job lease every RENEW_EVERY until `stop` flips. Uses
    /// the job-scoped token the job was issued with.
    fn spawn_renewer(
        &self,
        job: &JobMessage,
        stop: Arc<AtomicBool>,
    ) -> Option<std::thread::JoinHandle<()>> {
        let base = job.run_service_url().map(str::to_string)?;
        let auth = job
            .service_token()
            .map(|t| format!("Bearer {t}"))
            .or_else(|| self.token.value.clone().into());
        let job = job.clone();
        Some(std::thread::spawn(move || {
            let mut client = match Client::new() {
                Ok(c) => c,
                Err(_) => return,
            };
            client.set_bearer(auth.as_deref().unwrap_or(""));
            let mut waited = Duration::ZERO;
            while !stop.load(Ordering::SeqCst) {
                // Sleep in slices so `stop` is honoured promptly —
                // run_job joins this thread.
                std::thread::sleep(Duration::from_millis(500));
                waited += Duration::from_millis(500);
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if waited < RENEW_EVERY {
                    continue;
                }
                waited = Duration::ZERO;
                let url = format!("{}/renewjob", base.trim_end_matches('/'));
                let body = serde_json::json!({
                    "planId": job.plan_id,
                    "jobId": job.job_id,
                });
                if let Err(e) = client.post_json_raw(&url, &body) {
                    eprintln!("gha-runner: lease renewal failed: {e}");
                }
            }
        }))
    }
}

fn safe(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
