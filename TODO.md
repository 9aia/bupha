# TODO — next version (post-0.1.0)

- [ ] T-027 status signature/attestation link in commit-status body (ADR-0002 compliance) · status:in-progress · created:2026-10-31
- [ ] T-031 presentation deck — pending: run via ~/.9aia-pres pipeline (pres kick bupha or supervisor); agent writes presentation/ on branch presentation/initial, slidev build verify, push; no PR (sturn flow) — the pushed branch is the deliverable; merge is a human/sturn call; status board: ~/.9aia-pres/STATUS.md · created:2026-11-02
- [ ] T-032 agent-awareness managed block — bupha's repo contract (myco check in gate pipelines, Item: T-NNN commit trailers) should live in a \<!-- bupha:agent-awareness --> block written by init + validated by check — pattern: myco init/check · created:2026-11-02
- [ ] T-049 migrate queue/cache/lease impls out of bupha — redb/redis queue becomes hypha-side impl; bupha keeps thin port or none (ADR-0006) · pri:high · created:2026-11-10
- [ ] T-067 ci-ingress live e2e + normalization gaps — fire a real forge push through webhook→ci.enqueue→worker ci.claim→ci.complete on a deployed node; pull\_request lanes; per-repo indexDoc spec resolution (Config::watched → queue/runsOn/runner/context/maxAttempts); X-GitHub-Delivery passthrough; enqueue retry on http.failed · status:in-progress · pri:high · owner:devin · created:2026-11-11 · links:T-058,T-061,T-063
- [ ] T-048 gha runner follow-ups — parallel matrix, actions/setup-\* + other uses:, if: conditions, needs.\*.outputs, per-gha-job commit statuses, timeout-minutes enforcement, multi-commit paths filters (PL-013 D-deferred) · status:in-progress · owner:devin · created:2026-11-10 · #ci · #gha
- [ ] T-073 fork-PR head shas — ci-job indexDoc lacks clone\_url/PR head ref so fork pull\_requests can't be checked out from base clone (T-067 remainder) · created:2026-11-12
- [ ] T-044 rotate CARGO\_REGISTRY\_TOKEN — was at .env 0644 (now 600); treat as exposed · status:in-progress · pri:high · owner:human:Luis-Emidio · created:2026-11-03

## open
