# TODO — next version (post-0.1.0)

- [ ] T-027 status signature/attestation link in commit-status body (ADR-0002 compliance) · status:in-progress · created:2026-10-31
- [ ] T-031 presentation deck — pending: run via ~/.9aia-pres pipeline (pres kick bupha or supervisor); agent writes presentation/ on branch presentation/initial, slidev build verify, push; no PR (sturn flow) — the pushed branch is the deliverable; merge is a human/sturn call; status board: ~/.9aia-pres/STATUS.md · created:2026-11-02
- [ ] T-032 agent-awareness managed block — bupha's repo contract (myco check in gate pipelines, Item: T-NNN commit trailers) should live in a \<!-- bupha:agent-awareness --> block written by init + validated by check — pattern: myco init/check · created:2026-11-02
- [ ] T-044 rotate CARGO\_REGISTRY\_TOKEN — was at .env 0644 (now 600); treat as exposed · pri:high · created:2026-11-03
- [ ] T-049 migrate queue/cache/lease impls out of bupha — redb/redis queue becomes hypha-side impl; bupha keeps thin port or none (ADR-0006) · pri:high · created:2026-11-10
- [ ] T-067 ci-ingress live e2e + normalization gaps — fire a real forge push through webhook→ci.enqueue→worker ci.claim→ci.complete on a deployed node; pull\_request lanes; per-repo indexDoc spec resolution (Config::watched → queue/runsOn/runner/context/maxAttempts); X-GitHub-Delivery passthrough; enqueue retry on http.failed · pri:high · owner:devin · created:2026-11-11 · links:T-058,T-061,T-063
- [ ] T-066 gha-runner live validation — run against a real repo's self-hosted runner (registration token → session → acquirejob → completejob → results service) on github.com; check GHES api-version drift · status:blocked · created:2026-11-11 · links:T-050

## open

- [ ] T-069 gha: wire step outputs to consumers — gha-runner JobOutcome.outputs is stored but unsent (no completejob field; ${{ }} exprs are service-precompiled); bupha-gha dialect collects $GITHUB\_OUTPUT raw to $BUPHA\_GHA\_TMP/outputs.txt. Both need a path to needs.\*.outputs / steps.\*.outputs (T-048 follow-up) · created:2026-11-11
