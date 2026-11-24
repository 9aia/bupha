# Changelog

## [0.3.0] - 2026-11-24

### Added

- per-repo [repos.runner] override + scripts/release.sh (`f407e50`)
- [cache] config section + RunHandle on Runner port (`cab5eac`)
- bupha-ssh — split sync/run stages, per-run remote dirs, stdin token (T-005) (`9da4dc7`)
- tests + publish metadata + quickstart (`983e62b`)
- bupha-gha runner — .github/workflows on.push as CI dialect (`722e85d`)
- bupha deploy subcommand + crate renames + robustness fixes (`1ec2665`)

### Changed

- impl-owned runner config (RunnerCfg → kind + extra table) (`2eee42b`)

### Fixed

- **dagger:** forward patch_host; emit only declared runner flags (`e59763a`)
