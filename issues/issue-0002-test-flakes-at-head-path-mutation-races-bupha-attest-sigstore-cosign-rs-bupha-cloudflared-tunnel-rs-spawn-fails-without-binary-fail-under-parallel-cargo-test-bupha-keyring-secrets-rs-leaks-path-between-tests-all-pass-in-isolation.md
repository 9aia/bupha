+++
id = "I-002"
title = "test flakes at HEAD — PATH-mutation races: bupha-attest-sigstore/cosign.rs, bupha-cloudflared/tunnel.rs::spawn_fails_without_binary fail under parallel cargo test; bupha-keyring/secrets.rs leaks PATH between tests; all pass in isolation"
status = "done"
created = "2026-11-03"
owner = "devin"
priority = "med"
+++

# test flakes at HEAD — PATH-mutation races: bupha-attest-sigstore/cosign.rs, bupha-cloudflared/tunnel.rs::spawn_fails_without_binary fail under parallel cargo test; bupha-keyring/secrets.rs leaks PATH between tests; all pass in isolation

Status: **open** · Created: 2026-11-03

## Description

What is wrong and steps to reproduce.

## Checklist

- [ ] Action item

## Resolution

Fixed in 804e98a: env-mutating test files now serialize behind a Mutex with PATH/HOME restore guards (pattern from bupha-ssh); deploy_env_variants uses a guaranteed-empty PATH dir; agent_error_exits retries the port hold-bind (free_port TOCTOU). cargo test --workspace and nextest both green.
