//! Reporting: step states to the results service as they happen,
//! logs at the end, and the job's conclusion to the run service.
//! Everything here is best-effort — a results-service hiccup must
//! not kill a job that already ran; failures are logged to stderr.

use std::collections::BTreeMap;

use crate::client::Client;
use crate::exec::Progress;
use crate::types::*;

/// Live reporter: implements [`Progress`] so the executor can push
/// step starts/finishes and log lines; `finish` uploads everything
/// and closes the job out.
pub struct Reporter<'a> {
    client: &'a Client,
    job: &'a JobMessage,
    offer: Option<&'a JobOffer>,
    results_url: Option<String>,
    change_order: u64,
    /// Steps in wire shape so far.
    steps: Vec<StepResult>,
    /// Current step's captured log (keyed by position until finish).
    current: Option<usize>,
    logs: BTreeMap<usize, String>,
    /// step backend id → log, harvested at finish.
    step_logs: BTreeMap<String, String>,
}

impl<'a> Reporter<'a> {
    pub fn new(client: &'a Client, job: &'a JobMessage, offer: Option<&'a JobOffer>) -> Self {
        Self {
            client,
            job,
            offer,
            results_url: job.results_url().map(str::to_string),
            change_order: 0,
            steps: Vec::new(),
            current: None,
            logs: BTreeMap::new(),
            step_logs: BTreeMap::new(),
        }
    }

    /// After the job: upload per-step + whole-job logs, push the
    /// final step states, and report the outcome. Best-effort —
    /// errors go to stderr, never propagate (the job already ran).
    pub fn finish(&mut self, outcome: Outcome) {
        if let Some(url) = self.results_url.clone() {
            self.harvest_logs();
            for (step_id, log) in self.step_logs.clone() {
                if log.is_empty() {
                    continue;
                }
                if let Err(e) = self.client.upload_log(self.job, &url, Some(&step_id), &log) {
                    eprintln!("gha-runner: cannot upload step log ({step_id}): {e}");
                }
            }
            let whole: String = self.step_logs.values().cloned().collect();
            if !whole.is_empty() {
                if let Err(e) = self.client.upload_log(self.job, &url, None, &whole) {
                    eprintln!("gha-runner: cannot upload job log: {e}");
                }
            }
        }
        self.push_steps();
        if let Err(e) = self
            .client
            .complete_job(self.offer, self.job, outcome, &self.steps)
        {
            eprintln!(
                "gha-runner: cannot report job outcome ({:?}): {e}",
                outcome.name()
            );
        }
    }

    fn harvest_logs(&mut self) {
        for (pos, result) in self.steps.iter().enumerate() {
            if let Some(log) = self.logs.remove(&pos) {
                self.step_logs.insert(result.external_id.clone(), log);
            }
        }
    }

    fn push_steps(&mut self) {
        let Some(url) = self.results_url.clone() else {
            return;
        };
        self.change_order += 1;
        if let Err(e) = self
            .client
            .update_steps(self.job, &url, self.change_order, &self.steps)
        {
            eprintln!("gha-runner: cannot update step states: {e}");
        }
    }
}

impl Progress for Reporter<'_> {
    fn step_started(&mut self, pos: usize, _step: &PipelineStep) {
        self.current = Some(pos);
        self.logs.entry(pos).or_default();
    }

    fn line(&mut self, line: &str) {
        if let Some(pos) = self.current {
            self.logs.entry(pos).or_default().push_str(line);
        }
    }

    fn step_finished(&mut self, _pos: usize, result: &StepResult) {
        self.steps.push(result.clone());
        self.current = None;
        self.push_steps();
    }

    fn flush(&mut self) {}
}
