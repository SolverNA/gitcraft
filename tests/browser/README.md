# Real Chromium acceptance

These tests launch the Rust web server as a subprocess and control a real
Chromium browser through Playwright. They do not substitute a DOM mock.

```sh
cargo build --locked
npm ci --ignore-scripts
npx playwright install chromium
npm run test:browser
```

On a supported Ubuntu CI runner, install browser OS dependencies explicitly with
`npx playwright install --with-deps chromium`. The project workflow includes
this step. Chromium and `node_modules` are downloaded dependencies, never Git
artifacts. Use `GITCRAFT_BROWSER_BINARY=/absolute/path/gitcraft` to select an
already built binary. A fixture always creates its own temporary configuration.

Coverage includes:

- Cookie login/logout/relogin and write-only credential rotation.
- Folder upload, observable progress, failed HTTP chunk, reload, file reselection
  and resumed offsets using the same scoped token/session.
- Whole-file verification, explicit world replacement, refusal while active,
  stopped replacement with backup and interrupted-world recovery.
- Successful JAR upload/import and reviewed cleanup preserving imported cache.
- Approved config editing, escaped diff, rejection of secrets, local review
  branch publication and PR retry after a simulated GitHub failure.
- Full settings preview/apply, offline backup and bootstrap consent/refusal.
- The 64 MiB browser limit and the displayed streaming CLI alternative.

The endpoint is real HTTP loopback, equivalent to the local endpoint exposed by
an SSH tunnel. Tests do **not** establish an SSH connection or contact a real
VPS. RCON responses, privileged service calls and GitHub CLI are fixture tools;
Git commits/pushes go only to a temporary local bare repository. Successful
Minecraft startup and real systemd launcher discovery remain integration checks
for a staging host. Browser bootstrap checks consent and safe refusal when the
fixture unit is unavailable. No production credentials or game data are used.

Traces are disabled to avoid retaining scoped tokens/credential form contents.
Temporary worlds, JAR bytes, settings, local Git repositories and receipts are
removed after each test; test outputs are Git-ignored.
