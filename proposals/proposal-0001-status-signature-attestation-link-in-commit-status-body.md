+++
id = "P-001"
title = "status signature/attestation link in commit-status body"
status = "open"
created = "2026-10-31"
owner = "devin"
priority = "medium"
+++

Implements T-027 / ADR-0002 §5 line: 'Status sink gets a signature link field (the attestation URL)'.

## Problem

The Attest port (landed: bupha-attest-file, bupha-attest-sigstore) writes a signed predicate per run, but nothing links it from the commit status — the compliance story ends at a file on disk. Reviewers clicking the GitHub check should reach the attestation.

## Design

1. Extend StatusSink::post with an optional link: `fn post(&self, sha, state, desc, link: Option<&str>)` — GitHubStatus maps it to the `target_url` field of the statuses API (unset = today). LogStatus prints it.
2. The run path already calls `attest.attest(&RunAttestation)` -> `io::Result<PathBuf>`. The returned path is local; the link needs a URL. Options:
   a. [attest] `url_base` config — `url_base/<filename>` published path (sigstore registry, an internal artifact host, or the history/audit HTTP surface later). Minimal: file impl keeps file:// semantics and the link is emitted only when url_base is set.
   b. Push the predicate to a registry (sigstore bundle upload) — bigger; phase 2.
   Recommend (a): one config field, honest about what it links to.
3. Ordering: post status BEFORE attest? No — attest first (the run already finished by then; the predicate needs the outcome), then the single status post carries the link. Pending/error posts keep link=None.
4. Failure semantics: attestation failure must not break the run — post status without link (same posture as history/telemetry write-paths).

## Open questions

- link on *every* status for the sha or only the terminal one? (Simplest: terminal only.)
- GHES compat of target_url — it is core statuses API, fine.
- Should the desc mention the predicate filename too for log readers?

## Effort

Trait signature touch (all impls), one config field, wire in run_job settle path. ~half a day.
