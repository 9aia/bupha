# BACKLOG — horizon

Ideas parked, not scheduled. Pull into project/TODO.md when picked up.

## Runners / triggers
- [ ] Remote/distributed runners: `bupha-webhook` runner (POST signed
      job to another bupha) + `bupha-http` trigger (generic signed job
      endpoint, not github-shaped) — multi-arch builds on worker
      boxes; also unblocks release.sh's "other arches" dist step.
- [ ] `runner.kind = "shell"` — inline shell strings (vs script file
      `cmd`), or alias; decide semantics.
- [ ] Trigger impls: gitea/gitlab.
- [ ] Runner impls: earthly, nix.

## Hardening
- [ ] `tiny_http` serves serially — a stalled connection blocks the
      accept loop (slowloris-shaped). Acceptable behind cloudflared;
      revisit if `direct` exposure becomes common.

## Housekeeping
- [ ] CHANGELOG.md generated per `bupha-v*` tag (start manual with the
      0.1.0 entry; automate later).
- [ ] crates.io + license badges in README once published.
