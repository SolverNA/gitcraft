#!/usr/bin/env bash
# Execute the first deployment with local root-owned secrets, without secret argv.
set -euo pipefail
accept=false
dry_run=false
while (($#)); do
  case "$1" in
    --accept-eula) accept=true; shift ;;
    --dry-run) dry_run=true; shift ;;
    --help) printf '%s\n' 'sudo scripts/bootstrap-server.sh --accept-eula [--dry-run]' 'Prerequisite: block external TCP RCON access (IPv4 and IPv6) before starting Minecraft.'; exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
if [[ "$accept" != true ]]; then printf '%s\n' 'Explicit --accept-eula is required.' >&2; exit 2; fi
if [[ "$dry_run" == true ]]; then
  printf '%s\n' 'Plan: read root-owned local secrets without executing the file; run explicit bootstrap as minecraft.' \
    'Plan: shared bootstrap activates only the verified standard poller after successful initialization; activation_required is partial success and must be retried without reinitializing.' \
    'Prerequisite: block external RCON for IPv4 and IPv6 before real bootstrap.'
  exit 0
fi
if ((EUID != 0)); then printf '%s\n' 'Run with sudo; local secrets are root-readable only.' >&2; exit 2; fi
while IFS='=' read -r name value; do
  value=${value#\"}; value=${value%\"}
  case "$name" in
    MINECRAFT_RCON_PASSWORD|GITCRAFT_WEB_TOKEN)
      if [[ ! "$value" =~ ^[a-fA-F0-9]{64}$ ]]; then printf '%s\n' 'Local secrets must be 64-character hexadecimal values.' >&2; exit 2; fi
      export "$name=$value"
      ;;
    ''|'#'*) ;;
    *) printf '%s\n' 'Unexpected setting in secrets.env.' >&2; exit 2 ;;
  esac
done < /etc/gitcraft/secrets.env
export GITCRAFT_SSH_CONFIG=/etc/gitcraft/ssh.conf
runuser -u minecraft -- /opt/gitcraft/bin/gitcraft bootstrap --config /etc/gitcraft/config/agent.yml --accept-eula
# The same bootstrap backend used by CLI/Web validates and activates the exact poller.
# A partial-success JSON result means Minecraft initialized but agent activation needs retry.
# Preserve this root helper's existing Minecraft boot-autostart behavior. Poller
# activation itself is performed exclusively by the shared initialization backend.
service=$(/opt/gitcraft/bin/gitcraft status --config /etc/gitcraft/config/agent.yml | jq -er '.service')
if [[ ! "$service" =~ ^[A-Za-z0-9][A-Za-z0-9_.@-]*\.service$ ]]; then printf '%s\n' 'Invalid configured Minecraft service.' >&2; exit 2; fi
systemctl enable "$service"
