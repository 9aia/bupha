# bupha

*The oxpecker on your repo's back.* Headless CI: watch pushes, batch
latest-wins, run a pluggable runner per sha, report statuses. No
server, no UI — one binary + one config file.

## Layout

    crates/              rust workspace
      bupha-core         Queue + bupha.toml + Trigger/StatusSink ports
      bupha-runner       runner port    -> impls: bupha-dagger, bupha-script
      bupha-tunnel       tunnel port    -> impls: bupha-cloudflared
      bupha-github       webhook verify + commit-status impl
      bupha-cli          the `bupha` binary
      bupha-deploy       install-time helpers
    modules/dagger/      shared dagger module (optional pipeline payload)
    examples/            bupha.toml.example, env.example
    docs/                sccache/S3 backend notes

## Config

`bupha.toml` per repo (see examples/). Secrets via env only:
`BUPHA_WEBHOOK_SECRET`, `BUPHA_TOKEN`.

## Commands

    bupha init              scaffold dagger module in a repo
    bupha doctor            check what the config actually needs
    bupha watch             webhook -> latest-wins queue -> runner -> status
    bupha run [fn]          run the pipeline locally
    bupha deploy service    write + enable the systemd unit
    bupha deploy webhook    register the repo's push hook (gh api)
    bupha deploy env        seed ~/.config/bupha/env

## Swappable by design

runner (dagger | script | …) · trigger (github | …) · status sink
(github | log | none) · tunnel (none | cloudflared | direct) · cache
backend (any sccache env file — local garage/minio or R2/AWS).

Docker is only needed when `runner.kind = "dagger"`. The script runner
is the zero-container path: `cmd = "./ci.sh"` in bupha.toml.

## Queue semantics

One run in flight + one pending sha. Pushes during a run overwrite the
pending sha (superseded shas get an error status marked "superseded").
`cancel_running = true` also kills the in-flight run.

Queue is in-process: a push that lands while bupha is down is missed —
GitHub does not redeliver.

## Quickstart

    cargo install bupha                    # or: cargo build --release
    cd your-repo && bupha init             # dagger module + delegate
    bupha deploy env                       # seed ~/.config/bupha/env
    # fill BUPHA_WEBHOOK_SECRET + BUPHA_TOKEN, write bupha.toml
    #   (see examples/bupha.toml.example)
    bupha deploy service                   # systemd unit, enabled
    bupha deploy webhook                   # register push hook
    bupha deploy check                     # verify the whole install

## Distribution

`cargo install bupha` (crates.io). Lib crates publish in dependency
order: core → runner/tunnel ports → impls (dagger, script, github,
cloudflared, deploy) → cli package `bupha`.
