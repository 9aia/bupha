# Changelog

## [0.3.0] - 2026-11-24

### Added

- ForgeAdmin port (decouple deploy webhook) + bupha-ssh remote runner (`9e46fa9`)
- bupha-ssh — split sync/run stages, per-run remote dirs, stdin token (T-005) (`9da4dc7`)
- bupha-gha runner — .github/workflows on.push as CI dialect (`722e85d`)

### Changed

- impl-owned runner config (RunnerCfg → kind + extra table) (`2eee42b`)

### Fixed

- multi-watcher queue bugs — restore, scoped persist, done markers, claim-lost kill (T-005) (`2aa7398`)
