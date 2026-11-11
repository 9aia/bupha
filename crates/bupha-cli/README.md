# bupha-cli

*Part of [bupha](https://github.com/9aia/bupha) — the oxpecker on your repo's back.*

The `bupha` binary — **a dev/debug harness, not the product
surface** (ADR-0006 amendment: there is no user-facing bupha CLI).
Deployed fleets run the engine through `tamias-driver-bupha`'s
`host` verb under tamias provider units; users install/operate via
`tamias install`/`deploy`/`driver exec`. This binary remains for
local reproduction (`run`, `runs`, `explain`, `log`), box checks
(`doctor`), standalone/dev-mode watching (`watch`, `worker`,
`agent`, `deploy`/`destroy`/`service` helpers), and one-shot
migration (`import <campaign.yaml>`).
