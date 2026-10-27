#!/usr/bin/env bash
# Explicit GitHub write operation, using the administrator's gh authentication only.
set -euo pipefail
repo=''; tool_repo=''; tool_ref=''; create=false; dry_run=false
while (($#)); do
  case "$1" in
    --repository) repo=${2:?}; shift 2 ;;
    --tool-repository) tool_repo=${2:?}; shift 2 ;;
    --tool-ref) tool_ref=${2:?}; shift 2 ;;
    --create) create=true; shift ;;
    --dry-run) dry_run=true; shift ;;
    --help) printf '%s\n' 'bootstrap-repo.sh --repository OWNER/REPO --tool-repository OWNER/TOOL --tool-ref FULL_COMMIT_SHA [--create] [--dry-run]'; exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
for name in "$repo" "$tool_repo"; do
  if [[ ! "$name" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then printf '%s\n' 'Repository must be OWNER/NAME.' >&2; exit 2; fi
done
if [[ ! "$tool_ref" =~ ^[a-fA-F0-9]{40}$ ]]; then printf '%s\n' 'Tool ref must be a full reviewed 40-character commit SHA.' >&2; exit 2; fi
if [[ "$dry_run" == true ]]; then
  printf '%s\n' "Repository: $repo; create private repository: $create" \
    "Tool source: $tool_repo@$tool_ref" \
    'Plan: use local gh authentication; copy nonconflicting templates; set pinned tool variables.' \
    'Plan: create initial main only for --create; otherwise push setup branch and open PR.' \
    'Plan: never store GitHub credentials on the Minecraft server.'
  exit 0
fi
command -v gh >/dev/null
gh auth status
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
template_dir="$script_dir/../templates/config-repository"
temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT
if [[ "$create" == true ]]; then gh repo create "$repo" --private; fi
gh repo clone "$repo" "$temporary/repository"
cd "$temporary/repository"
if [[ "$create" == true ]]; then
  git checkout --orphan main
  branch=main
else
  branch="gitcraft/setup-$(date -u +%Y%m%d%H%M%S)"
  git checkout -b "$branch"
fi
while IFS= read -r -d '' file; do
  relative=${file#"$template_dir/"}
  if [[ -e "$relative" || -L "$relative" ]]; then
    # Existing user files are preserved; conflicting workflows require a manual review.
    if [[ "$relative" == .github/workflows/* ]]; then
      printf 'Refusing to overwrite existing workflow: %s\n' "$relative" >&2; exit 2
    fi
    continue
  fi
  mkdir -p -- "$(dirname -- "$relative")"
  cp -- "$file" "$relative"
done < <(find "$template_dir" -type f -print0)
git add -- .github requirements server-settings .gitignore README.md
if git diff --cached --quiet; then printf '%s\n' 'No template changes to propose.'; exit 0; fi
git -c user.name=gitcraft-setup -c user.email=gitcraft-setup@users.noreply.github.com commit -m 'chore: initialize Minecraft GitOps configuration'
git -c credential.helper='!gh auth git-credential' push origin "HEAD:refs/heads/$branch"
gh variable set GITCRAFT_TOOL_REPOSITORY --repo "$repo" --body "$tool_repo"
gh variable set GITCRAFT_TOOL_REF --repo "$repo" --body "$tool_ref"
if [[ "$create" != true ]]; then
  printf '%s\n' 'Adds Minecraft GitOps templates. Existing configuration files were preserved. Configure engine pins, branch protection and Actions PR permission before deployment.' > "$temporary/pr.md"
  gh pr create --repo "$repo" --head "$branch" --title 'Initialize Minecraft GitOps' --body-file "$temporary/pr.md"
fi
printf '%s\n' 'Repository configured. Enable protected main and required reviews; allow Actions PR creation if desired.'
