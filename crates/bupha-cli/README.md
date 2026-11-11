# bupha-cli

*Part of [bupha](https://github.com/9aia/bupha) — the oxpecker on your repo's back.*

The bupha engine lib — **a dev/debug harness; the deployed surface
is `tamias-driver-bupha`** (ADR-0006 amendment + P-021 + T-012).
Deployed fleets run the engine through the driver's `host`/`worker`
verbs under tamias provider units; users install/operate via
`tamias install`/`deploy`/`driver exec`.

There is **no `bupha` binary** — the command ships as the tamias
pack in `libraries/bupha`: a generated `bins[]` shim →
`tamias bin exec` → `tamias-driver-bupha cli --dir <pkg> --name
bupha <argv>` → this lib's `run_from` in-process with inherited
stdio (`scripts/pack.sh` stages the driver payload;
`tamias install --allow-drivers ./libraries/bupha` deploys the
shim).

The lib surface (`Cli`/`command()`/`parse_from()`/`run_from()`/
`dispatch`) is what `tamias-driver-bupha`'s ops manifest derives
from — every bounded verb is a `cli.<verb>` wire query,
`watch`/`agent`/`gha-runner` ride `surfaces`. The verbs exist for
local reproduction (`run`, `runs`, `explain`, `log`), box checks
(`doctor`), standalone/dev-mode watching (`watch`, `agent`,
`gha-runner`, `deploy`/`destroy`/`service` helpers — fleet workers
are the driver's `worker` verb, not a CLI verb), and one-shot
migration (`import <campaign.yaml>`). Engine e2e tests live in
`crates/bupha-driver/tests/` and spawn `tamias-driver-bupha run`.
