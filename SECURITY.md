# Security Policy

Report vulnerabilities privately to the maintainers via GitHub:
https://github.com/9aia/bupha/security/advisories/new

Do not open public issues for security reports.

## Scope

- `bupha watch` binds a webhook listener — HMAC verification of
  `X-Hub-Signature-256` is mandatory; requests are capped at 2 MiB and
  unauthenticated payloads never reach the runner.
- Secrets come from env vars only (`BUPHA_WEBHOOK_SECRET`,
  `BUPHA_TOKEN`); bupha never writes them to disk and never mounts them
  into runner processes beyond the runner's own documented mechanism
  (`env://` secret for dagger).
- The tunnel is a child process under supervision; failures are
  restarted, credentials stay in `~/.cloudflared/`.

We respond to reports as quickly as we can; expect acknowledgement
within a few days.
