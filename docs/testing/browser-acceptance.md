# Real browser acceptance — 2026-10-28

Executed locally with **Playwright 1.64.1** and an actual **Chromium
156.0.8078.4** headless process. Five application scenarios passed. This is
separate from the faster Node VM handler tests and Rust HTTP/unit tests.

## Reproduction

```sh
cargo build --locked
npm ci --ignore-scripts
npx playwright install chromium
npm run test:browser
```

The local build used `cargo +stable build --locked` with Rust 1.95.0. The
repository toolchain is pinned for ordinary `cargo` commands. On the Ubuntu
GitHub Actions runner the workflow explicitly runs
`npx playwright install --with-deps chromium`, then builds the binary and runs
the browser suite. Browser/package versions are pinned through `package.json`
and `package-lock.json`. Installation follows the [Playwright CLI
instructions](https://playwright.dev/docs/test-cli).

The local host was Manjaro; Playwright used its Ubuntu 24.04 fallback download.
Its required shared libraries were already available, so no OS packages or host
services were changed for this run. Dependencies and browser cache are not Git
artifacts.

## Observed scenarios

| Scenario | Actual browser actions and assertions |
|---|---|
| Login and size boundary | Log in, inspect HttpOnly/SameSite cookie, select a sparse file over 64 MiB, see rejection and exact CLI fallback before session creation, log out, observe protected-page denial, log in again. |
| Folder transfer and recovery | Select a real directory; abort a later HTTP chunk; observe nonzero progress/error; reload and reselect files; retain the same session/token and resume committed offsets without creating another session. Verify whole files; refuse unconfirmed/live-world replacement; replace a stopped fixture world with backup. Inject a prepared journal, refuse recovery while active, then restore the original files while stopped. Cancel the session and explicitly review/apply cleanup. |
| JAR file transfer | Select a real temporary JAR-named fixture file; verify and import its exact bytes; preview/review/apply cleanup; prove the imported SHA-addressed cache and cache receipt remain. Rust regression additionally loads a transfer manifest and successfully resolves/verifies this cache after session removal. |
| Config and PR retry | Open an approved config, reject a secret without echoing it, edit text containing a script/textarea payload, inspect escaped real diff, explicitly save only the authoring clone. Publish a review branch to a local bare Git origin; fail the first fake GitHub PR call; retry the same immutable branch/commit without a new push; keep deployment branch unchanged. |
| Operational controls | Preview all scalar settings and change polling through the form, prove config is unchanged before review, apply the reviewed settings, rotate a write-only Web token and invalidate the old login, create an offline fixture-world backup, enforce EULA consent and report a guarded bootstrap refusal for an unavailable fixture unit. |

Folder selection uses Chromium's real file input support documented by
[Playwright's locator API](https://playwright.dev/docs/api/class-locator#locator-set-input-files).
Transfers use browser WebCrypto and the actual Rust HTTP/chunk/verify/commit
routes; the tests do not replace the DOM, hashing implementation or server with
a JavaScript simulation.

## External boundaries

- Browser traffic reaches a real **loopback HTTP endpoint**. This is the local
  endpoint shape exposed by an SSH tunnel, but **no SSH tunnel was established**
  and no VPS was contacted. SSH authentication, routing and TLS are external
  staging checks.
- RCON responses and privileged service operations use deterministic local
  fixtures. Bootstrap with accepted EULA safely refuses the unavailable
  systemd unit before applying files or starting Java. Successful Minecraft
  startup and real systemd/D-Bus discovery are **not** proved by this browser
  run; core runtime tests cover the successful transaction logic.
- Git commands operate on temporary local repositories, with the GitHub-looking
  origin rewritten to a local bare repository. GitHub CLI is a fixture which
  deliberately fails once. No GitHub PR, remote push or account mutation occurs.
- World/JAR payloads are synthetic temporary fixture data, not a real Minecraft
  distribution. No production credentials are used. Traces are disabled so
  scoped tokens and credential form contents are not retained in recordings.
- Temporary settings, worlds, JAR bytes, receipts and local repositories are
  removed after tests. `node_modules`, browser reports, traces and build outputs
  are Git-ignored. The report contains no session tokens or credential values.

Tests live in [tests/browser](../../tests/browser/README.md); lifecycle and
integrity regressions also run in `cargo test --locked`.
