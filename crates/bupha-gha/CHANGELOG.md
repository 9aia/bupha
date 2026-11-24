# Changelog

## [0.3.0] - 2026-11-24

### Added

- **gha:** setup-java (temurin) + setup-dotnet toolchain lowerings (T-048) (`ea1737d`)
- **gha:** GITHUB_ENV/GITHUB_OUTPUT/GITHUB_PATH file commands in both executors (T-060) (`e0c60ca`)
- **gha:** parallel matrix — per-leg worktrees, pgid concurrency, fail-fast kills (T-048) (`dfe240d`)
- **gha:** wire step/job outputs to needs.*/steps.*.outputs consumers (T-069) (`b924fd2`)
- **gha:** if:/timeout-minutes/uses:/paths coverage — T-048 dialect gaps (`93f8e2a`)
- bupha-gha runner — .github/workflows on.push as CI dialect (`722e85d`)
- **gha:** per-job commit statuses — one status per expanded workflow job/matrix leg (`67f4bcb`)
- **gha:** setup-{node,python,go} + actions/cache* uses: lowering (T-048) (`4ec1338`)
- **gha:** actions/upload-artifact + download-artifact via per-run tar store (T-048) (`44d6f3c`)

### Changed

- **vcs:** consume lasius-vcs port — drop bupha-vcs/bupha-git (`c7c717d`)
