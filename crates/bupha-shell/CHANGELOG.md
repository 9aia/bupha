# Changelog

## [0.3.0] - 2026-11-24

### Added

- [cache] config section + RunHandle on Runner port (`cab5eac`)
- ForgeAdmin port (decouple deploy webhook) + bupha-ssh remote runner (`9e46fa9`)
- bupha-ssh — split sync/run stages, per-run remote dirs, stdin token (T-005) (`9da4dc7`)
- bupha-gha runner — .github/workflows on.push as CI dialect (`722e85d`)

### Changed

- impl-owned runner config (RunnerCfg → kind + extra table) (`2eee42b`)
- rename bupha-deploy→bupha-systemd, bupha-script→bupha-shell (`279080c`)

### Fixed

- **runner:** optional systemd-scope isolation for script jobs (`439c4a7`)
