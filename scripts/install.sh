#!/usr/bin/env bash
# Install from a reviewed checkout or a separately verified prebuilt Rust binary.
set -euo pipefail
umask 027
repo_url=''
binary=''
dry_run=false
os_release=/etc/os-release
while (($#)); do
  case "$1" in
    --repository) repo_url=${2:?missing repository URL}; shift 2 ;;
    --binary) binary=${2:?missing binary path}; shift 2 ;;
    --dry-run) dry_run=true; shift ;;
    --os-release) os_release=${2:?missing os-release path}; shift 2 ;;
    --help) printf '%s\n' 'Usage: install.sh --repository git@github.com:OWNER/REPO.git [--binary PATH] [--dry-run]' 'Default builds reviewed checkout with cargo build --locked --release.' 'First deployment is explicit: gitcraft bootstrap --config /etc/gitcraft/config/agent.yml --accept-eula'; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done
if [[ -z "$repo_url" && -t 0 ]]; then
  read -r -p 'Read-only repository SSH or HTTPS URL: ' repo_url
fi
# Never accept credential-bearing URLs or YAML/shell metacharacters.
if [[ ! "$repo_url" =~ ^git@github\.com:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(\.git)?$ && ! "$repo_url" =~ ^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(\.git)?$ ]]; then
  printf '%s\n' 'A GitHub SSH or credential-free HTTPS repository URL is required.' >&2; exit 2
fi
if [[ ! -r "$os_release" ]]; then printf '%s\n' 'Cannot determine Linux distribution.' >&2; exit 2; fi
# Parse os-release as data, never execute it. Alternate files are dry-run-only.
if [[ "$os_release" != /etc/os-release && "$dry_run" != true ]]; then
  printf '%s\n' '--os-release is only available with --dry-run.' >&2; exit 2
fi
ID=''; VERSION_ID=''
while IFS='=' read -r key value; do
  value=${value#\"}; value=${value%\"}; value=${value#\'}; value=${value%\'}
  case "$key" in ID) ID=$value ;; VERSION_ID) VERSION_ID=$value ;; esac
done < "$os_release"
case "$ID" in debian|ubuntu) ;; *) printf 'Unsupported distribution: %s (supported: Debian, Ubuntu with systemd)\n' "$ID" >&2; exit 2 ;; esac
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source_dir=$(cd -- "$script_dir/.." && pwd)
if [[ "$dry_run" == true ]]; then
  printf '%s\n' "Distribution: $ID $VERSION_ID" "Repository: $repo_url" \
    'Plan: install git, openssh-client, sudo, ca-certificates, openssl, Java 21 (or retain Java >=21).' \
    'Plan: install reviewed Rust binary to /opt/gitcraft/bin/gitcraft; create minecraft system user.' \
    'Plan: preserve /etc/gitcraft/config/agent.yml and secrets.env; create missing files with restricted permissions.' \
    'Plan: generate read-only deploy key; print public key for GitHub registration; never upload a writable token.' \
    'Plan: install systemd services and restricted sudoers; start localhost Web UI.' \
    'Plan: leave fresh Minecraft and deployment timer disabled until explicit bootstrap and EULA acceptance.' \
    'Plan: leave firewall and SSH configuration unchanged.'
  exit 0
fi
if ((EUID != 0)); then printf '%s\n' 'Run installation with sudo; use --dry-run to inspect without privileges.' >&2; exit 2; fi
if [[ ! -d /run/systemd/system ]]; then printf '%s\n' 'A running systemd host is required.' >&2; exit 2; fi
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends git openssh-client sudo ca-certificates openssl
java_major=0
if command -v java >/dev/null; then
  java_version=$(java -version 2>&1 | head -n 1)
  if [[ "$java_version" =~ \"([0-9]+) ]]; then java_major=${BASH_REMATCH[1]}; fi
fi
if ((java_major < 21)); then
  if ! apt-cache show openjdk-21-jre-headless >/dev/null 2>&1; then
    printf '%s\n' 'Java >=21 is required. This distribution has no openjdk-21-jre-headless package; install a suitable JRE, then rerun.' >&2; exit 2
  fi
  apt-get install -y --no-install-recommends openjdk-21-jre-headless
fi
if [[ -z "$binary" ]]; then
  if ! command -v cargo >/dev/null; then printf '%s\n' 'Supply --binary PATH or install a Rust toolchain and run cargo build --locked --release.' >&2; exit 2; fi
  (cd "$source_dir" && cargo build --locked --release)
  binary="$source_dir/target/release/gitcraft"
fi
if [[ ! -f "$binary" || ! -x "$binary" ]]; then printf '%s\n' 'Rust binary must be an executable regular file.' >&2; exit 2; fi
if ! id minecraft >/dev/null 2>&1; then useradd --system --create-home --home-dir /var/lib/minecraft --shell /usr/sbin/nologin minecraft; fi
install -d -m 0755 /opt/gitcraft/bin
install -m 0755 "$binary" /opt/gitcraft/bin/gitcraft
install -d -m 0750 -o root -g minecraft /etc/gitcraft
install -d -m 0700 -o minecraft -g minecraft /etc/gitcraft/config
for directory in repository server state cache staging backups; do
  install -d -m 0750 -o minecraft -g minecraft "/var/lib/minecraft/$directory"
done
if [[ ! -e /etc/gitcraft/config/agent.yml ]]; then
  cat > /etc/gitcraft/config/agent.yml <<EOF
repository:
  url: '$repo_url'
  branch: main
  poll_interval_seconds: 60
paths:
  repository: /var/lib/minecraft/repository
  server: /var/lib/minecraft/server
  state: /var/lib/minecraft/state
  cache: /var/lib/minecraft/cache
  staging: /var/lib/minecraft/staging
  backups: /var/lib/minecraft/backups
deployment:
  service: minecraft.service
  empty_grace_seconds: 60
  health_timeout_seconds: 180
rcon:
  host: 127.0.0.1
  port: 25575
  password_env: MINECRAFT_RCON_PASSWORD
  timeout_seconds: 5
web:
  host: 127.0.0.1
  port: 8765
  token_env: GITCRAFT_WEB_TOKEN
EOF
  chown minecraft:minecraft /etc/gitcraft/config/agent.yml
  chmod 0600 /etc/gitcraft/config/agent.yml
fi
if [[ ! -e /etc/gitcraft/secrets.env ]]; then
  # Hex avoids systemd EnvironmentFile escaping and Minecraft properties escaping.
  printf 'MINECRAFT_RCON_PASSWORD=%s\nGITCRAFT_WEB_TOKEN=%s\n' "$(openssl rand -hex 32)" "$(openssl rand -hex 32)" > /etc/gitcraft/secrets.env
  chown root:root /etc/gitcraft/secrets.env
  chmod 0600 /etc/gitcraft/secrets.env
fi
if [[ ! -e /etc/gitcraft/java.env ]]; then
  printf '%s\n' 'JAVA_OPTS=-Xms1G -Xmx2G' > /etc/gitcraft/java.env
  chmod 0644 /etc/gitcraft/java.env
fi
install -d -m 0700 -o minecraft -g minecraft /var/lib/minecraft/.ssh
if [[ ! -e /var/lib/minecraft/.ssh/id_ed25519 ]]; then
  runuser -u minecraft -- ssh-keygen -q -t ed25519 -N '' -C gitcraft-read-only -f /var/lib/minecraft/.ssh/id_ed25519
fi
# Host keys are deliberately not trusted via unauthenticated ssh-keyscan.
install -m 0644 "$source_dir/systemd/minecraft.service" /etc/systemd/system/minecraft.service
install -m 0644 "$source_dir/systemd/gitcraft-agent.service" /etc/systemd/system/gitcraft-agent.service
install -m 0644 "$source_dir/systemd/gitcraft-agent.timer" /etc/systemd/system/gitcraft-agent.timer
install -m 0644 "$source_dir/systemd/gitcraft-web.service" /etc/systemd/system/gitcraft-web.service
temporary_sudoers=$(mktemp)
trap 'rm -f -- "$temporary_sudoers"' EXIT
printf '%s\n' 'minecraft ALL=(root) NOPASSWD: /usr/bin/systemctl start minecraft.service, /usr/bin/systemctl stop minecraft.service, /usr/bin/systemctl is-active minecraft.service' > "$temporary_sudoers"
visudo -cf "$temporary_sudoers"
install -m 0440 "$temporary_sudoers" /etc/sudoers.d/gitcraft
systemctl daemon-reload
systemctl enable --now gitcraft-web.service
printf '%s\n' 'Installation complete. Existing configuration and runtime data were preserved.' \
  'Register this public key in the configuration repository as a deploy key WITHOUT write access:'
cat /var/lib/minecraft/.ssh/id_ed25519.pub
printf '%s\n' 'Verify GitHub SSH host fingerprints independently, then populate /var/lib/minecraft/.ssh/known_hosts.' \
  'Web UI: ssh -L 8765:127.0.0.1:8765 USER@HOST; open http://127.0.0.1:8765.' \
  'Read Web UI token locally from /etc/gitcraft/secrets.env (it is not printed).' \
  'After repository pins are configured, explicitly bootstrap with EULA acceptance; see README.' \
  'Then: systemctl enable --now minecraft.service gitcraft-agent.timer' \
  'Firewall unchanged. RCON must be blocked on external interfaces; expose Minecraft only as intended.'
