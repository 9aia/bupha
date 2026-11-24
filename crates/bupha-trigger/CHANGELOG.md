# Changelog

## [0.3.0] - 2026-11-24

### Added

- pull_request deliveries + bupha import campaign (`f8c1b78`)
- Source port — queue engine's ingest edge (T-033) (`e850e98`)
- [cache] config section + RunHandle on Runner port (`cab5eac`)
- ForgeAdmin port (decouple deploy webhook) + bupha-ssh remote runner (`9e46fa9`)
- ADR-0002 — access port (+file/forge adapters), telemetry port (+otel), container runner, attest+lease+history ports (`516604a`)
- per-repo webhook secrets — [[repo]] secret_env falls back to trigger.secret_env (`2123f6e`)

### Changed

- extract bupha-trigger + bupha-status port crates (`3600059`)
