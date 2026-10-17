# BACKLOG — horizon

Ideas parked, not scheduled. Pull into project/TODO.md when picked up.

## Docs / site
- [ ] Auto-deploy docs on docs-branch changes (and on repo changes
      when the API surface moves — via the dogfood loop).

## Migration
- [ ] `bupha migrate` — one-shot GHA workflow → `ci.sh`+`bupha.toml`
      converter (ADR-0001).

## Runners / triggers
- [ ] `runner.kind = "shell"` — inline shell strings vs `cmd` file;
      decide semantics.
- [ ] Trigger impls: gitea/gitlab.
- [ ] Runner impls: earthly, nix.
- [ ] Feature-gated impls in the binary (`--features kafka`) once a
      heavy impl exists — ports already allow third-party impl crates
      outside this repo.

## Hardening
- [ ] `tiny_http` serves serially — stalled connection blocks the
      accept loop (slowloris-shaped). Acceptable behind cloudflared;
      revisit if `direct` exposure becomes common.
