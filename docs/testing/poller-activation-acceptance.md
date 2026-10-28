# Poller activation acceptance

Checked on 2026-10-28 with Rust 1.95.0. Run:

```sh
cargo test --locked activation_acceptance
```

Three tests cover the activation gap:

- `http_bootstrap_and_adoption_activate_shared_poller_and_apply_next_merge`: exercises both new-server bootstrap and existing-server adoption through an actual loopback HTTP listener. Login cookies, CSRF, explicit EULA refusal, adoption plan digest/review and background operation outcomes use the production handlers. Initialization writes actual deployment state and files. The shared activation orchestrator starts the shared production polling scheduler. A subsequent branch change merged into the configured release branch of a real temporary Git repository is applied automatically, with no second manual deploy request. Repeated activation does not spawn another poller.
- `http_activation_failure_preserves_initialization_and_retry_starts_polling`: repeats bootstrap and adoption while the external activation boundary refuses startup. The HTTP operation reports `activation_required`, initialized state remains durable, and no polling occurs. Confirmed activation retry starts polling without repeating initialization and applies the pending approved merge.
- `shared_scheduler_reloads_configured_interval_without_restart`: changes the configured interval from one to two seconds during reconciliation, then checks the observed configurations and actual elapsed intervals of the same running scheduler.

The activation fixture acquires the real deployment lock before spawning the scheduler and checks that initialized state already exists. This proves that activation follows initialization and lock release. Threads have explicit stop signals and are joined; temporary Git repositories, synthetic artifact bytes, cache receipts and runtime directories are removed with the fixture.

These are Rust HTTP integration tests, not browser automation. Systemd inspection, installation file trust, enable/start, Minecraft process status and RCON are injected external boundaries. Git commits, clones, reviewed branch merges, manifest policy, SHA-256 checks, managed-file writes, durable ownership state, activation orchestration, HTTP authorization and the polling scheduler run normally. No host service was enabled, no Minecraft process or VPS was used, and no GitHub request or SSH tunnel was needed. The separate [browser acceptance report](browser-acceptance.md) describes actual Chromium coverage and its boundaries.
