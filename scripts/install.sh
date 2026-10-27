#!/usr/bin/env bash
# Install a reviewed Rust binary. --mode adopt preserves the existing Minecraft unit and credentials.
set -euo pipefail
umask 027
repo_url=''; binary=''; dry_run=false; os_release=/etc/os-release
mode=new; service=minecraft.service; server_dir=/var/lib/minecraft/server; java_path=/usr/bin/java; engine=server.jar
runtime_user=minecraft; runtime_group=minecraft; secret_env=MINECRAFT_RCON_PASSWORD; rcon_port=25575
while (($#)); do
  case "$1" in
    --repository) repo_url=${2:?}; shift 2 ;;
    --binary) binary=${2:?}; shift 2 ;;
    --mode) mode=${2:?}; shift 2 ;;
    --service) service=${2:?}; shift 2 ;;
    --server-path) server_dir=${2:?}; shift 2 ;;
    --java-path) java_path=${2:?}; shift 2 ;;
    --engine) engine=${2:?}; shift 2 ;;
    --rcon-port) rcon_port=${2:?}; shift 2 ;;
    --rcon-password-env) secret_env=${2:?}; shift 2 ;;
    --dry-run) dry_run=true; shift ;;
    --os-release) os_release=${2:?}; shift 2 ;;
    --help) printf '%s\n' 'install.sh --repository git@github.com:OWNER/REPO.git [--binary PATH] [--dry-run]' '--mode new: optional --service NAME.service --server-path ABS --java-path ABS --engine NAME.jar' '--mode adopt: --service EXISTING.service --server-path EXISTING_ABS_PATH [--rcon-password-env ENV]' 'Adopt preserves existing unit/User/Java/runtime files and requires current RCON password (env or hidden prompt).'; exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
if [[ -z "$repo_url" && -t 0 ]]; then read -r -p 'Read-only GitHub repository URL: ' repo_url; fi
if [[ ! "$repo_url" =~ ^git@github\.com:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ && ! "$repo_url" =~ ^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then printf '%s\n' 'Credential-free GitHub HTTPS or SSH URL required.' >&2; exit 2; fi
[[ "$mode" == new || "$mode" == adopt ]] || { printf '%s\n' 'Mode must be new or adopt.' >&2; exit 2; }
[[ "$service" =~ ^[A-Za-z0-9][A-Za-z0-9_.@-]*\.service$ ]] || { printf '%s\n' 'Invalid service name.' >&2; exit 2; }
for value in "$server_dir" "$java_path"; do [[ "$value" =~ ^/[A-Za-z0-9_./-]+$ && "$value" != / && "$value" != *'/../'* && "$value" != */.. ]] || { printf '%s\n' 'Paths must be absolute, without spaces, symlinks or parent traversal.' >&2; exit 2; }; done
[[ "$rcon_port" =~ ^[0-9]+$ && "$rcon_port" -ge 1 && "$rcon_port" -le 65535 ]] || { printf '%s\n' 'Invalid RCON port.' >&2; exit 2; }
[[ "$engine" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*\.jar$ && "$secret_env" =~ ^[A-Z_][A-Z0-9_]*$ ]] || { printf '%s\n' 'Invalid engine filename or environment reference.' >&2; exit 2; }
if [[ "$os_release" != /etc/os-release && "$dry_run" != true ]]; then printf '%s\n' '--os-release requires --dry-run.' >&2; exit 2; fi
ID=''; VERSION_ID=''
while IFS='=' read -r key value; do value=${value#\"}; value=${value%\"}; value=${value#\'}; value=${value%\'}; case "$key" in ID) ID=$value ;; VERSION_ID) VERSION_ID=$value ;; esac; done < "$os_release"
case "$server_dir" in /etc|/etc/*|/usr|/usr/*|/bin|/bin/*|/sbin|/sbin/*|/proc|/proc/*|/sys|/sys/*|/dev|/dev/*|/run|/run/*|/boot|/boot/*) printf '%s\n' 'Server data must not be placed in a protected system tree.' >&2; exit 2 ;; esac
case "$ID" in debian|ubuntu) ;; *) printf 'Unsupported distribution: %s\n' "$ID" >&2; exit 2 ;; esac
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source_dir=$(cd -- "$script_dir/.." && pwd)
if [[ "$dry_run" == true ]]; then
  printf '%s\n' "Distribution: $ID $VERSION_ID" "Mode: $mode; service: $service; server: $server_dir" "Repository: $repo_url" \
    'Plan: install Rust binary, read-only deploy SSH identity, localhost authenticated UI and config-driven long-lived agent.' \
    'Plan: preserve config, secrets, worlds and runtime data; never overwrite an unmanaged Minecraft unit.'
  if [[ "$mode" == adopt ]]; then printf '%s\n' 'Plan: discover existing unit User/Group/WorkingDirectory; preserve existing Java/ExecStart/RCON credentials.' 'Plan: no new Minecraft unit, no Java installation, no Minecraft stop/start, no agent enable until reviewed adopt.'; else printf '%s\n' "Plan: validate Java at $java_path, create managed unit only if absent or previously managed." 'Plan: leave Minecraft and agent disabled until explicit EULA bootstrap.'; fi
  printf '%s\n' 'Plan: leave firewall and SSH configuration unchanged.'
  exit 0
fi
((EUID==0)) || { printf '%s\n' 'Run with sudo or inspect --dry-run.' >&2; exit 2; }
[[ -d /run/systemd/system ]] || { printf '%s\n' 'A running systemd host is required.' >&2; exit 2; }
reject_symlinks(){ local path=$1 current='' part; local -a parts; IFS='/' read -r -a parts <<< "$path"; for part in "${parts[@]}"; do [[ -n "$part" ]] || continue; current="$current/$part"; [[ ! -L "$current" ]] || { printf 'Refusing installation symlink: %s\n' "$current" >&2; exit 2; }; done; }
for path in /opt/gitcraft/bin/gitcraft /etc/gitcraft/config/agent.yml /etc/gitcraft/secrets.env /etc/gitcraft/java.env /etc/gitcraft/ssh.conf /var/lib/minecraft/.ssh/id_ed25519 /var/lib/minecraft/.ssh/id_ed25519.pub /etc/sudoers.d/gitcraft /etc/systemd/system/gitcraft-agent.service /etc/systemd/system/gitcraft-web.service "/etc/systemd/system/$service" "$server_dir"; do reject_symlinks "$path"; done
for directory in repository state cache staging backups; do reject_symlinks "/var/lib/minecraft/$directory"; done
if [[ "$mode" == adopt ]]; then
  runtime_user=$(systemctl show --value --property=User "$service")
  [[ "$runtime_user" =~ ^[a-z_][a-z0-9_-]*\$?$ && "$runtime_user" != root ]] || { printf '%s\n' 'Adopt requires an existing service with explicit non-root Unix User.' >&2; exit 2; }
  id "$runtime_user" >/dev/null
  runtime_group=$(systemctl show --value --property=Group "$service")
  if [[ -z "$runtime_group" ]]; then runtime_group=$(id -gn "$runtime_user"); fi
  [[ "$runtime_group" =~ ^[a-z_][a-z0-9_-]*\$?$ ]] || { printf '%s\n' 'Invalid runtime group.' >&2; exit 2; }
  existing_dir=$(systemctl show --value --property=WorkingDirectory "$service")
  [[ -d "$server_dir" && "$(realpath -- "$existing_dir")" == "$(realpath -- "$server_dir")" ]] || { printf '%s\n' 'Explicit server path must match existing unit WorkingDirectory.' >&2; exit 2; }
else
  fragment=$(systemctl show --value --property=FragmentPath "$service" 2>/dev/null || true)
  if [[ -n "$fragment" ]] && ! grep -Fq '# Managed by GitCraft new installation' "$fragment"; then printf '%s\n' 'Existing unmanaged Minecraft unit detected. Use --mode adopt; no unit was overwritten.' >&2; exit 2; fi
fi
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends git openssh-client sudo ca-certificates openssl jq
if [[ "$mode" == new ]]; then
  java_major=0
  if [[ -x "$java_path" ]]; then java_version=$("$java_path" -version 2>&1 | head -n 1); if [[ "$java_version" =~ \"([0-9]+) ]]; then java_major=${BASH_REMATCH[1]}; fi; fi
  if ((java_major<17)); then
    [[ "$java_path" == /usr/bin/java ]] || { printf '%s\n' 'Explicit Java executable must exist and be Java >=17.' >&2; exit 2; }
    apt-cache show openjdk-21-jre-headless >/dev/null 2>&1 || { printf '%s\n' 'Install suitable Java >=17 at explicit --java-path; Java21 package unavailable.' >&2; exit 2; }
    apt-get install -y --no-install-recommends openjdk-21-jre-headless
  fi
fi
if [[ -z "$binary" ]]; then command -v cargo >/dev/null || { printf '%s\n' 'Supply --binary PATH or install the pinned Rust toolchain.' >&2; exit 2; }; (cd "$source_dir" && cargo build --locked --release); binary="$source_dir/target/release/gitcraft"; fi
[[ -f "$binary" && -x "$binary" ]] || { printf '%s\n' 'Executable Rust binary required.' >&2; exit 2; }
if ! id "$runtime_user" >/dev/null 2>&1; then useradd --system --create-home --home-dir /var/lib/minecraft --shell /usr/sbin/nologin "$runtime_user"; fi
install -d -m 0755 /opt/gitcraft/bin
install -m 0755 "$binary" /opt/gitcraft/bin/gitcraft
install -d -m 0750 -o root -g "$runtime_group" /etc/gitcraft
install -d -m 0700 -o "$runtime_user" -g "$runtime_group" /etc/gitcraft/config
install -d -m 0750 -o "$runtime_user" -g "$runtime_group" /var/lib/minecraft
for directory in repository state cache staging backups; do install -d -m 0750 -o "$runtime_user" -g "$runtime_group" "/var/lib/minecraft/$directory"; done
if [[ "$mode" == new ]]; then install -d -m 0750 -o "$runtime_user" -g "$runtime_group" "$server_dir"; fi
if [[ ! -e /etc/gitcraft/config/agent.yml ]]; then
cat > /etc/gitcraft/config/agent.yml <<CONFIG
repository:
  url: '$repo_url'
  branch: main
  poll_interval_seconds: 60
paths:
  repository: /var/lib/minecraft/repository
  server: $server_dir
  state: /var/lib/minecraft/state
  cache: /var/lib/minecraft/cache
  staging: /var/lib/minecraft/staging
  backups: /var/lib/minecraft/backups
deployment:
  service: $service
  empty_grace_seconds: 60
  health_timeout_seconds: 180
rcon:
  host: 127.0.0.1
  port: $rcon_port
  password_env: MINECRAFT_RCON_PASSWORD
  timeout_seconds: 5
web:
  host: 127.0.0.1
  port: 8765
  token_env: GITCRAFT_WEB_TOKEN
CONFIG
  chown "$runtime_user:$runtime_group" /etc/gitcraft/config/agent.yml; chmod 0600 /etc/gitcraft/config/agent.yml
fi
if [[ ! -e /etc/gitcraft/secrets.env ]]; then
  if [[ "$mode" == adopt ]]; then
    rcon_secret=${!secret_env:-}
    if [[ -z "$rcon_secret" && -t 0 ]]; then read -r -s -p 'Existing RCON password (preserved, not reset): ' rcon_secret; printf '\n'; fi
    [[ -n "$rcon_secret" && "$rcon_secret" != *$'\n'* && "$rcon_secret" != *$'\r'* ]] || { printf '%s\n' 'Supply the EXISTING RCON password through the chosen env or hidden prompt. Credentials were not reset.' >&2; exit 2; }
  else rcon_secret=$(openssl rand -hex 32); fi
  # systemd EnvironmentFile double-quoted syntax; never evaluate secrets as shell.
  escaped=${rcon_secret//\\/\\\\}; escaped=${escaped//\"/\\\"}
  printf 'MINECRAFT_RCON_PASSWORD="%s"\nGITCRAFT_WEB_TOKEN=%s\n' "$escaped" "$(openssl rand -hex 32)" > /etc/gitcraft/secrets.env
fi
chown root:root /etc/gitcraft/secrets.env; chmod 0600 /etc/gitcraft/secrets.env
if [[ "$mode" == new && ! -e /etc/gitcraft/java.env ]]; then printf '%s\n' 'JAVA_OPTS=-Xms1G -Xmx2G' > /etc/gitcraft/java.env; chmod 0644 /etc/gitcraft/java.env; fi
install -d -m 0700 -o "$runtime_user" -g "$runtime_group" /var/lib/minecraft/.ssh
if [[ ! -e /var/lib/minecraft/.ssh/id_ed25519 ]]; then runuser -u "$runtime_user" -- ssh-keygen -q -t ed25519 -N '' -C gitcraft-read-only -f /var/lib/minecraft/.ssh/id_ed25519; fi
cat > /etc/gitcraft/ssh.conf <<'SSHCONFIG'
Host github.com
  IdentityFile /var/lib/minecraft/.ssh/id_ed25519
  UserKnownHostsFile /var/lib/minecraft/.ssh/known_hosts
  IdentitiesOnly yes
  BatchMode yes
  StrictHostKeyChecking yes
SSHCONFIG
chmod 0644 /etc/gitcraft/ssh.conf
if [[ "$mode" == new ]]; then
  { printf '%s\n' '# Managed by GitCraft new installation'; sed -e "s|User=minecraft|User=$runtime_user|" -e "s|Group=minecraft|Group=$runtime_group|" -e "s|WorkingDirectory=/var/lib/minecraft/server|WorkingDirectory=$server_dir|" -e "s|/usr/bin/java |$java_path |" -e "s|server.jar nogui|$engine nogui|" -e "s|ReadWritePaths=/var/lib/minecraft/server|ReadWritePaths=$server_dir|" "$source_dir/systemd/minecraft.service"; } > "/etc/systemd/system/$service"
fi
for unit in gitcraft-agent.service gitcraft-web.service; do
  sed -e "s|User=minecraft|User=$runtime_user|" -e "s|Group=minecraft|Group=$runtime_group|" -e "s|ReadWritePaths=/var/lib/minecraft$|ReadWritePaths=/var/lib/minecraft $server_dir|" -e "s|ReadWritePaths=/etc/gitcraft/config /var/lib/minecraft$|ReadWritePaths=/etc/gitcraft/config /var/lib/minecraft $server_dir|" "$source_dir/systemd/$unit" > "/etc/systemd/system/$unit"
done
# Migrate the old timer only: a single long-lived agent reads the configured interval.
systemctl disable --now gitcraft-agent.timer >/dev/null 2>&1 || true
sudoers=$(mktemp); trap 'rm -f -- "$sudoers"' EXIT
printf '%s ALL=(root) NOPASSWD: /usr/bin/systemctl start %s, /usr/bin/systemctl stop %s, /usr/bin/systemctl is-active %s\n' "$runtime_user" "$service" "$service" "$service" > "$sudoers"
visudo -cf "$sudoers"; install -m 0440 "$sudoers" /etc/sudoers.d/gitcraft
systemctl daemon-reload
systemctl enable --now gitcraft-web.service
printf '%s\n' 'Installed. Existing configuration, credentials, runtime data and unmanaged Minecraft unit were preserved.' 'Register the following public key as a READ-ONLY repository deploy key:'
cat /var/lib/minecraft/.ssh/id_ed25519.pub
printf '%s\n' 'Verify GitHub host fingerprints independently, populate /var/lib/minecraft/.ssh/known_hosts.' 'SSH UI tunnel: ssh -L 8765:127.0.0.1:8765 USER@VPS; browser http://127.0.0.1:8765.' 'Read UI token locally from root-only /etc/gitcraft/secrets.env. Block external RCON IPv4+IPv6.'
if [[ "$mode" == adopt ]]; then printf '%s\n' 'Review gitcraft adopt plan/apply first; no existing Minecraft stop/start or credential reset was performed.' 'After successful adoption: systemctl enable --now gitcraft-agent.service'; else printf '%s\n' 'Pin/import engine and review config, then explicit EULA bootstrap; see scripts/bootstrap-server.sh.'; fi
