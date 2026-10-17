# Security Policy

Report vulnerabilities privately to the maintainers via GitHub:
https://github.com/9aia/bupha/security/advisories/new

Do not open public issues for security reports.

## Scope

- `bupha watch` binds a webhook listener — HMAC verification of
  `X-Hub-Signature-256` is mandatory; requests are capped at 2 MiB and
  unauthenticated payloads never reach the runner.
- Secrets (`BUPHA_WEBHOOK_SECRET`, `BUPHA_TOKEN`) resolve via env var
  → OS keyring (`secret-tool`) → `~/.config/bupha/env` (chmod 600);
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

We respond to reports as quickly as we can; expect acknowledgement
within a few days.
