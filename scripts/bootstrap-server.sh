#!/usr/bin/env bash
# Execute the first deployment with local root-owned secrets, without secret argv.
set -euo pipefail
accept=false
dry_run=false
autostart=''
while (($#)); do
  case "$1" in
    --accept-eula) accept=true; shift ;;
    --dry-run) dry_run=true; shift ;;
    --autostart) autostart=${2:?missing enabled or disabled policy}; shift 2 ;;
    --help) printf '%s\n' 'sudo scripts/bootstrap-server.sh --accept-eula [--autostart enabled|disabled] [--dry-run]' 'Prerequisite: block external TCP RCON access (IPv4 and IPv6) before starting Minecraft.'; exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
if [[ -n "$autostart" && "$autostart" != enabled && "$autostart" != disabled ]]; then printf '%s\n' 'Unsupported autostart policy.' >&2; exit 2; fi
if [[ "$accept" != true ]]; then printf '%s\n' 'Explicit --accept-eula is required.' >&2; exit 2; fi
if [[ "$dry_run" == true ]]; then
  printf '%s\n' 'Plan: read root-owned local secrets without executing the file; run explicit bootstrap as minecraft.' \
    'Plan: shared bootstrap activates only the verified standard poller after successful initialization; activation_required is partial success and must be retried without reinitializing.' \
    'Prerequisite: block external RCON for IPv4 and IPv6 before real bootstrap.'
  if [[ -n "$autostart" ]]; then printf 'Plan: explicitly set Minecraft boot policy to %s without starting/restarting it.\n' "$autostart"; else printf '%s\n' 'Minecraft boot policy remains unchanged; explicit enabled/disabled selection is still required.'; fi
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
# Poller activation is shared with CLI/Web. Minecraft boot policy is an independent explicit choice.
if [[ -n "$autostart" ]]; then
  runuser -u minecraft -- /opt/gitcraft/bin/gitcraft minecraft-autostart --config /etc/gitcraft/config/agent.yml --policy "$autostart" --confirmed
else
  printf '%s\n' 'Minecraft boot policy was not changed. Choose enabled/disabled explicitly through UI or minecraft-autostart before relying on OS reboot startup.'
fi
