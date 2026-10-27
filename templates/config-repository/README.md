# Minecraft desired state

Only reviewed configuration and exact dependency pointers belong here. Worlds,
runtime databases, JARs, logs and secrets remain on the server.

Before first deployment, add `requirements/engine.json`: a JSON object with
`name`, `filename` equal to `server.jar`, and `pinned` containing the exact
`version`, credential-free HTTPS `url`, and full 64-character `sha256`. Obtain
the actual release and verify its checksum; this template deliberately contains
no fake or moving download URL. Plugins use the same objects in the `plugins`
array, mods in the `mods` array; their filenames must be plain JAR basenames.

Direct pins do not auto-update. To opt a dependency into stable Modrinth updates,
add `source` with `provider: "modrinth"`, the `project` ID/slug, exact `minecraft`
version, `loader` (such as `paper` or `fabric`), and `channel: "release"`.
The currently pinned version must be present among compatible releases. The bot
verifies the provider SHA-512, computes SHA-256, and opens a PR. Minecraft version
and loader are never changed automatically. A release can still be incompatible
with other plugins; human approval and backups remain necessary.

`GITCRAFT_TOOL_REPOSITORY` and `GITCRAFT_TOOL_REF` repository variables identify
the reviewed tool source and a full 40-character commit SHA. The setup script
sets these. For dependency PRs allow GitHub Actions to create pull requests in
repository Settings → Actions → General. Configure protected `main` and required
reviews/checks yourself; organization policy may constrain these settings.

Do not put RCON passwords in `server.properties`: the agent writes local secrets
after applying Git configuration. Bootstrap on the host requires explicit EULA
acceptance. The agent never pushes deployment logs back into this repository.

Modrinth API: https://docs.modrinth.com/api/operations/getprojectversions/
GitHub Actions permissions: https://docs.github.com/en/actions/concepts/security/github_token
