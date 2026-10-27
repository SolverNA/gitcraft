#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT
printf '%s\n' 'ID=debian' 'VERSION_ID="13"' > "$temporary/os-release"
bash -n "$script_dir/install.sh"
bash -n "$script_dir/bootstrap-repo.sh"
bash -n "$script_dir/bootstrap-server.sh"
"$script_dir/install.sh" --repository git@github.com:team/config.git --dry-run --os-release "$temporary/os-release" > "$temporary/plan"
grep -Fq 'firewall and SSH configuration unchanged' "$temporary/plan"
# os-release content is data, including attempted command substitutions.
printf 'ID=debian\nVERSION_ID="$(touch %s/sentinel)"\n' "$temporary" > "$temporary/os-release"
"$script_dir/install.sh" --repository https://github.com/team/config.git --dry-run --os-release "$temporary/os-release" >/dev/null
test ! -e "$temporary/sentinel"
printf '%s\n' 'ID=ubuntu' 'VERSION_ID="24.04"' > "$temporary/os-release"
"$script_dir/install.sh" --repository git@github.com:team/config.git --dry-run --os-release "$temporary/os-release" > "$temporary/plan"
grep -Fq 'Distribution: ubuntu 24.04' "$temporary/plan"
"$script_dir/install.sh" --repository git@github.com:team/config.git --mode adopt --service paper.service --server-path /srv/paper --dry-run --os-release "$temporary/os-release" > "$temporary/adopt-plan"
grep -Fq 'preserve existing Java/ExecStart/RCON credentials' "$temporary/adopt-plan"
grep -Fq 'no Minecraft stop/start' "$temporary/adopt-plan"
if "$script_dir/install.sh" --repository git@github.com:team/config.git --mode broken --dry-run --os-release "$temporary/os-release" >/dev/null 2>&1; then exit 1; fi
if "$script_dir/install.sh" --repository git@github.com:team/config.git --service '--bad.service' --dry-run --os-release "$temporary/os-release" >/dev/null 2>&1; then exit 1; fi
printf '%s\n' 'ID=arch' > "$temporary/os-release"
if "$script_dir/install.sh" --repository git@github.com:team/config.git --dry-run --os-release "$temporary/os-release" >/dev/null 2>&1; then exit 1; fi
if "$script_dir/install.sh" --repository https://secret@github.com/team/config.git --dry-run >/dev/null 2>&1; then exit 1; fi
"$script_dir/bootstrap-repo.sh" --repository team/config --tool-repository team/tool --tool-ref aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --create --dry-run >/dev/null
if "$script_dir/bootstrap-repo.sh" --repository team/config --tool-repository team/tool --tool-ref main --dry-run >/dev/null 2>&1; then exit 1; fi
"$script_dir/bootstrap-server.sh" --accept-eula --dry-run >/dev/null
if "$script_dir/bootstrap-server.sh" --dry-run >/dev/null 2>&1; then exit 1; fi
printf '%s\n' 'Installer and GitHub setup dry-run checks passed.'
