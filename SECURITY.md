# Security Policy

Report vulnerabilities privately to the maintainers via GitHub:
https://github.com/9aia/bupha/security/advisories/new

Do not open public issues for security reports.

## Scope

- The watcher engine (run by `tamias-driver-bupha host` under
  tamias units, or `bupha watch` in standalone/dev mode — ADR-0007)
  binds a webhook listener — HMAC verification of
  `X-Hub-Signature-256` is mandatory; requests are capped at 2 MiB and
  unauthenticated payloads never reach the runner.
- Secrets (`BUPHA_WEBHOOK_SECRET`, `BUPHA_TOKEN`, `PEREGRIN_TOKEN`,
  `BUPHA_WH_*`) resolve via env var
  → cuora (`cuora://bupha/<NAME>`, canonical per T-056; every fleet
  secret migrated there in T-059) → OS keyring
  (`secret-tool`) → `~/.config/bupha/env` (chmod 600, retired —
  legacy code fallback only);
  values never appear in config files and are passed to runners only
  via the runner's own documented mechanism (`env://` for dagger).
- The tunnel is a child process under supervision; failures are
  restarted, credentials stay in `~/.cloudflared/`.

## Notes

Request bodies are capped (2 MiB), HMAC checks are constant-time,
`X-GitHub-Delivery` replays are deduped (bounded 10k ring), the pushed
sha must be hex before it touches worktree paths or status URLs,
superseded/cancelled shas get honest statuses, and `bupha.toml`
rejects unknown fields. See `project/TODO.md` for remaining hardening.

Additional hardening (security/audit-fixes):

- Shas are validated (`is_valid_sha`) on every path that reaches a
  filesystem or argv: push ingest, persisted-queue restore, fleet
  claim, the agent's job spec, and `run_job` itself — plus a leading
  `-` is rejected (git option injection). Repo names/job keys get the
  same treatment (`is_valid_key`); `status_repo` and `[[repo]] name`
  outside the allowlist fail config load.
- The `bupha agent` wire signs `METHOD PATH\nBODY` — a captured
  signature replays only against the exact request, never as a
  bearer token. Note the wire is plain HTTP: run it over loopback,
  a tailnet, or an SSH tunnel.
- Agent job specs validate `sha`/`key`/`env_names` before the tar
  extract dir is joined — a malformed spec cannot escape
  `worktree.base`.
- sturn pushes' `sender` is self-asserted until signed: when
  `sturn.toml [signing] allowed_signers` (or
  `~/.config/sturn/allowed_signers`) exists, events are verified via
  `ssh-keygen -Y verify` (`sturn-event` namespace); unsigned or
  bad-signature events emit an unattested (empty) sender.
- The ssh runner allowlists the remote (`user@host` charset, no
  leading `-`), sanitizes key/sha-derived dir+tag components, and
  validates `token_env` as an env-var name.
- `deploy env`/`deploy check`/`doctor` flag secret-bearing files
  (`~/.config/bupha/env`, the sccache conf) readable by group/other,
  and env files are created `0600` atomically.
- The webhook listener caps in-flight request threads (64 → 503)
  and accepts only POST on `/hook`.

We respond to reports as quickly as we can; expect acknowledgement
within a few days.
