# Minecraft desired state

Only reviewed configuration and exact dependency pointers belong here. Worlds,
runtime databases, JAR bytes, logs and secrets remain outside Git.

Prefer `gitcraft export plan/apply` to initialize this repository from a local
installation. Select each config explicitly, inspect the plan and the actual
source files, and apply only after review. Unknown JAR provenance is represented
as an explicitly chosen transfer pin, never an invented download URL.

## Layout and policy

- `server-settings/NAME` → top-level runtime config.
- `plugins-configs/PLUGIN/PATH` → `plugins/PLUGIN/PATH`.
- `config/PATH` and `defaultconfigs/PATH` retain the same runtime path.
- `requirements/{engine,plugins,mods}.json` contain exact dependency pins.
- `export-policy.json` lists exact `approved_config_paths` for arbitrary plugin,
  mod and other configs after manual review. Standard server.properties,
  bukkit.yml, spigot.yml and known engine YAML filenames are allowed by policy.

The same Rust policy runs during export, CLI validation, PR creation, CI and
server staging. It audits tracked files even if gitignore would hide them.
Do not put world YAML, plugin userdata, databases or credentials in config
folders. Secret detection is heuristic; human inspection is still necessary.

## Dependency pins

Before deployment, add `requirements/engine.json`: a JSON object with `name`, a
plain `filename` ending in `.jar`, and `pinned.version` plus a full SHA-256.
Choose one of:

- `pinned.url`: credential-free HTTPS URL for exactly these bytes.
- `pinned.transfer: true`, no URL, `source.provider: "transfer"`: JAR bytes must
  be independently transported and verified into the server's receipt-backed
  cache before deployment. A hash in Git alone does not import a local file.

Fresh installer defaults to engine filename `server.jar`; another basename
requires matching `install.sh --engine`. Adoption requires the existing unit's
actual JAR basename. Plugins use objects in the `plugins` array, mods in `mods`.
No moving or fake release URL is included in this template.

Direct and transfer pins do not auto-update. To opt into stable Modrinth updates,
set `source.provider: "modrinth"`, `project`, exact `minecraft`, `loader` and
`channel: "release"`. Keep a real pinned URL/version/hash; the current version
must be among compatible releases. The bot verifies provider SHA-512, computes
SHA-256 and opens a PR. It never changes the Minecraft version/loader itself.

## GitHub and deployment

`GITCRAFT_TOOL_REPOSITORY` and `GITCRAFT_TOOL_REF` repository variables select the
reviewed tool source and full 40-character commit SHA. The setup script sets
these. Current template checkout expects public tool source; private tool
source needs separately authorized cross-repository authentication.

Configure protection, required reviews/checks on the same deployment branch as
`agent.yml repository.branch`. The validation workflow runs for every push and
PR; the agent still watches only the configured deployment branch. Allow Actions
PR creation in Settings → Actions → General if enabling the updater. The updater
is a proposal workflow, not permission to auto-merge.

Do not store RCON passwords here: the agent supplies local secrets after config
application. Fresh bootstrap requires explicit EULA acceptance. Adoption
preserves existing credentials. The agent does not push deployment logs here.

See the [GitCraft operator wiki](https://github.com/SolverNA/gitcraft/wiki) for
export, transfer, adoption, authoring, security and stopped recovery guides.
