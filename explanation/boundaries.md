# Boundaries

bupha owns **CI gating** — push/PR ingest, queueing, worktree
isolation, pipeline execution, forge status write-back. A CI
server product (UI, YAML DSL beyond the `gha` dialect, managed
fleet) is explicitly out of scope. Neighbors:

| neighbor | owns | relationship |
| --- | --- | --- |
| **tamias** | distribution | bupha is a `type: bupha` content integration — packs, drivers, provider units |
| **peregrin** | orchestration | production forge ingress is a peregrin campaign listener normalizing deliveries into `ci-job` entities (ADR-0007) |
| **hypha** | substrate | the entity plane; `ci-job` durable queue state lives there via `bupha-plane` |
| **cuora** | secrets | `cuora://bupha/<NAME>` is the canonical secret store |
| **sturn** | coordination | the WAL source reads `refs/sturn/wal/*`; fleet claims |
| **myco** | work memory | this repo's `project/` ledger; `Item: T-NNN` trailers |
| **kuda/mimus** | code memory / critic | this repo's own cards and review |
