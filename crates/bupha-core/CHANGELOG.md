# Changelog

## [0.3.0] - 2026-11-24

### Added

- pull_request deliveries + bupha import campaign (`f8c1b78`)
- per-repo [repos.runner] override + scripts/release.sh (`f407e50`)
- shared pull queue + bupha worker + label routing (T-005) (`e22f602`)
- [queue] kind selects redb|redis; live redis tests pass (`e03d201`)
- runs|log|explain read paths + attest-file + queue-redis (`d075e35`)
- [cache] config section + RunHandle on Runner port (`cab5eac`)
- [[repo.jobs]] — multiple pipelines per repo (T-004) (`caa6473`)
- sturn-topic trigger — CI from WAL events, webhooks unaffected (`c35840d`)
- **bundles:** worker service in ci-gate/docs-deploy + [worker] node config (T-064) (`bded75e`)
- **core:** [[repo]] retry field — failed runs re-enqueue at back (T-024) (`b18a78e`)
- ForgeAdmin port (decouple deploy webhook) + bupha-ssh remote runner (`9e46fa9`)
- tests + publish metadata + quickstart (`983e62b`)
- **ci-job:** fork-PR head checkout via cloneUrl/headRef (T-073) (`882a92c`)
- per-repo queue policy (latest|all) + de-handl the module (`83258a7`)
- config port (bupha-config) + toml impl; core = pure domain (`72fe3d0`)
- multi-repo routing — [[repos]] watched repos (`60898f8`)
- **plane:** fleet normalization at ci-job create — with_watched specs resolve queue/runsOn/runner/context/maxAttempts, gate ref+event (T-067) (`5f560da`)
- ADR-0002 — access port (+file/forge adapters), telemetry port (+otel), container runner, attest+lease+history ports (`516604a`)
- fleet claim wiring + sigstore adapter + forge-access live tests (`3ba59b2`)
- **status:** attestation link on terminal commit status (T-027, ADR-0002 §5) (`2f8a064`)
- **worker:** tamias-driver-bupha worker — ci-job entity-plane claim loop (P-002, T-062) (`2cdcafb`)
- per-repo webhook secrets — [[repo]] secret_env falls back to trigger.secret_env (`2123f6e`)
- bupha deploy subcommand + crate renames + robustness fixes (`1ec2665`)
- **audit:** bupha-audit-file sink — decision/outcome JSONL log (T-025) (`1bd67f9`)
- bupha.yaml config — YAML loader, ~ path expansion, TOML stays (`1543079`)
- harden webhook intake — sha validation, dedup, timeouts (`14d3664`)
- GHES api_base + dir canonicalize + per-kind doctor + init skeleton (`0888519`)
- bupha-http remote runner + bupha agent (T-005) (`0640a68`)

### Changed

- [[repo]] (singular, TOML convention); drop legacy [repo] (`f96b0e5`)
- restructure into bupha monorepo (`5b5606d`)
- extract bupha-trigger + bupha-status port crates (`3600059`)
- impl-owned runner config (RunnerCfg → kind + extra table) (`2eee42b`)
- rename bupha-deploy→bupha-systemd, bupha-script→bupha-shell (`279080c`)
- replay dedup on the Trigger port (bupha_core::Seen) (`19134ea`)
- **queue:** retire redb/redis/lease queue impls; entity worker ref gate (T-049, T-068) (`09033ca`)

### Fixed

- path-safe revision validation + per-crate release tags (`d0ac5f9`)
- default webhook listener to :29173 — :9000 collides with common services (`b55db21`)
- per-request threads in listener (slowloris) + rustdoc warnings (`a2d1e6d`)
