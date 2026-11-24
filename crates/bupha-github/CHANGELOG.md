# Changelog

## [0.3.0] - 2026-11-24

### Added

- pull_request deliveries + bupha import campaign (`f8c1b78`)
- per-repo [repos.runner] override + scripts/release.sh (`f407e50`)
- Source port — queue engine's ingest edge (T-033) (`e850e98`)
- [cache] config section + RunHandle on Runner port (`cab5eac`)
- ForgeAdmin port (decouple deploy webhook) + bupha-ssh remote runner (`9e46fa9`)
- tests + publish metadata + quickstart (`983e62b`)
- **gha:** per-job commit statuses — one status per expanded workflow job/matrix leg (`67f4bcb`)
- multi-repo routing — [[repos]] watched repos (`60898f8`)
- bupha destroy + deploy release — lifecycle symmetry (`5ec0750`)
- ADR-0002 — access port (+file/forge adapters), telemetry port (+otel), container runner, attest+lease+history ports (`516604a`)
- harden the five user-facing snag points (`2fd0f79`)
- **status:** attestation link on terminal commit status (T-027, ADR-0002 §5) (`2f8a064`)
- per-repo webhook secrets — [[repo]] secret_env falls back to trigger.secret_env (`2123f6e`)
- bupha deploy subcommand + crate renames + robustness fixes (`1ec2665`)
- harden webhook intake — sha validation, dedup, timeouts (`14d3664`)
- GHES api_base + dir canonicalize + per-kind doctor + init skeleton (`0888519`)

### Changed

- restructure into bupha monorepo (`5b5606d`)
- extract bupha-trigger + bupha-status port crates (`3600059`)
- replay dedup on the Trigger port (bupha_core::Seen) (`19134ea`)

### Fixed

- review round 2 — security + lifecycle + dedup (`779f7ef`)
