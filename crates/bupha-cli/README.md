# bupha-cli

*Part of [bupha](https://github.com/9aia/bupha) — the oxpecker on your repo's back.*

The `bupha` binary — **a dev/debug harness; the engine lib is the
product surface** (ADR-0006 amendment + P-021). Deployed fleets run
the engine through `tamias-driver-bupha`'s `host` verb under tamias
provider units; users install/operate via `tamias
install`/`deploy`/`driver exec`. This crate is lib + thin bin: the
lib (`Cli`/`command()`/`parse_from()`/`run_from()`) is what
`tamias-driver-bupha`'s ops manifest derives from — every bounded
verb is a `cli.<verb>` wire query, `watch`/`agent`/`gha-runner`
ride `surfaces`. The binary remains for local reproduction (`run`,
`runs`, `explain`, `log`), box checks (`doctor`),
standalone/dev-mode watching (`watch`, `worker`, `agent`,
`deploy`/`destroy`/`service` helpers), and one-shot migration
(`import <campaign.yaml>`).
