---
layout: home

hero:
  name: bupha
  text: the oxpecker on your repo's back
  tagline: headless CI — push in, status out, on a Linux box with Git
  actions:
    - theme: brand
      text: Getting Started
      link: /getting-started
    - theme: alt
      text: Tutorial
      link: /tutorials/first-run

features:
  - icon: ⚡
    title: Push → status in seconds
    details: HMAC-verified webhooks, delivery dedup, honest pending/success/cancelled statuses — no YAML, no queue wait
    link: /tutorials/first-run
    linkText: first run
  - icon: 🏠
    title: Your box, your cache
    details: no runner minutes; build caches on your own disk or your S3/R2/minio via sccache
    link: /how-to/sccache-backend
    linkText: cache backends
  - icon: 🔀
    title: Two queue modes
    details: latest-wins for CI gates, bounded FIFO for releases — per repo; superseded shas never silently drop
    link: /explanation/architecture
    linkText: queue semantics
  - icon: 🌳
    title: Detached worktrees
    details: every run is an isolated git worktree of the pushed sha — never a shared, dirty checkout
  - icon: 🧩
    title: Ports, not frameworks
    details: runner (dagger | shell | ssh), tunnel (cloudflared | direct | none), trigger + status — swap via config or a new adapter crate
    link: /explanation/architecture
    linkText: architecture
  - icon: 🔐
    title: Secrets stay secret
    details: env var → OS keyring → 0600 env file; config holds names, never values; tokens never reach argv
    link: /getting-started
    linkText: setup
---
