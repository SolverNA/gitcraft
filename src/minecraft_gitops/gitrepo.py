"""Read-only remote fetch and immutable staging snapshots."""
from pathlib import Path
import re
import subprocess


def git(*args):
    return subprocess.run(['git', *map(str, args)], check=True, capture_output=True, text=True, timeout=120).stdout.strip()


def snapshot(url, branch, repository, staging):
    if not isinstance(url, str) or url.startswith('-') or not (url.startswith(('https://', 'ssh://', 'git@'))):
        raise ValueError('Repository must use HTTPS or SSH')
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._/-]*', branch) or '..' in branch or branch.endswith('/'):
        raise ValueError('Invalid Git branch')
    repository, staging = Path(repository), Path(staging)
    repository.parent.mkdir(parents=True, exist_ok=True)
    if not repository.exists():
        git('init', '--bare', repository)
    git('-C', repository, 'fetch', '--no-tags', '--', url, 'refs/heads/' + branch)
    commit = git('-C', repository, 'rev-parse', '--verify', 'FETCH_HEAD^{commit}')
    target = staging / commit
    staging.mkdir(parents=True, exist_ok=True)
    if target.exists():
        # Always recreate: stale/partial staging must never be trusted.
        import shutil
        shutil.rmtree(target)
    git('clone', '--no-checkout', '--', repository, target)
    git('-C', target, 'checkout', '--detach', commit)
    return commit, target
