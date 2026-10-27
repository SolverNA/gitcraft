"""Transactional deployment of explicitly managed files; worlds are never synchronized."""
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tempfile
import time

from . import artifacts, gitrepo
from .rcon import RconClient


def atomic_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(dir=path.parent, prefix='.state-')
    try:
        with os.fdopen(descriptor, 'w') as stream:
            json.dump(value, stream, indent=2)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


@contextmanager
def lock(path):
    with Path(path).open('a') as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('Another deployment is running') from None
        yield


def safe_path(root, relative):
    root = Path(root)
    relative = PurePosixPath(relative)
    if relative.is_absolute() or not relative.parts or any(p in ('.', '..') for p in relative.parts):
        raise ValueError('Unsafe managed path')
    candidate = root
    for part in relative.parts:
        candidate = candidate / part
        if candidate.is_symlink():
            raise ValueError('Symlinks are forbidden in managed paths')
    if root.is_symlink() or not candidate.resolve().is_relative_to(root.resolve()):
        raise ValueError('Managed path escapes server directory')
    return candidate


def managed_config_files(repo):
    result = {}
    for folder, prefix in [('server-settings', ''), ('plugins-configs', 'plugins/')]:
        base = Path(repo) / folder
        if base.is_symlink():
            raise ValueError('Config directory cannot be a symlink')
        if not base.exists():
            continue
        for source in base.rglob('*'):
            if source.is_symlink():
                raise ValueError('Config symlinks are forbidden')
            if source.is_dir():
                continue
            relative = source.relative_to(base)
            if source.suffix.lower() not in ('.yml', '.yaml', '.properties', '.json', '.txt', '.toml', '.conf'):
                raise ValueError('Unsupported managed configuration extension')
            if folder == 'server-settings' and (len(relative.parts) != 1 or source.name in ('eula.txt',)):
                raise ValueError('Server settings must be top-level config files')
            result[prefix + relative.as_posix()] = source
    return result


def systemctl(action, service):
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.@-]*\.service', service):
        raise ValueError('Invalid systemd service')
    return subprocess.run(['sudo', '-n', 'systemctl', action, service], check=True, capture_output=True, timeout=180)


def wait_healthy(client, service, timeout):
    deadline = time.monotonic() + timeout
    while True:
        try:
            systemctl('is-active', service)
            client.online()
            return
        except Exception:
            if time.monotonic() >= deadline:
                raise RuntimeError('Minecraft failed readiness check') from None
            time.sleep(min(2, max(0, deadline - time.monotonic())))


def copy_atomic(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(dir=destination.parent, prefix='.gitops-')
    try:
        with os.fdopen(descriptor, 'wb') as output, source.open('rb') as stream:
            shutil.copyfileobj(stream, output)
        os.chmod(temporary, 0o644)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)


def _run(config):
    paths = {key: Path(value) for key, value in config['paths'].items()}
    for key in ('state', 'cache', 'staging', 'backups', 'server'):
        paths[key].mkdir(parents=True, exist_ok=True)
    state_file = paths['state'] / 'deployment.json'
    state = json.loads(state_file.read_text()) if state_file.exists() else {}
    commit, repo = gitrepo.snapshot(config['repository']['url'], config['repository'].get('branch', 'main'), paths['repository'], paths['staging'])
    if commit == state.get('commit'):
        return {'status': 'unchanged', 'commit': commit}
    sources = managed_config_files(repo)
    manifest = artifacts.load_manifest(repo)
    if not any(a.destination == 'server.jar' for a in manifest):
        raise ValueError('An engine manifest is required before deployment')
    for artifact in manifest:
        if artifact.destination in sources:
            raise ValueError('Artifact/config destination collision')
        sources[artifact.destination] = artifacts.download(artifact, paths['cache'])
    # Validate every destination before stopping the server.
    previous = state.get('managed_files', [])
    if not isinstance(previous, list) or any(not isinstance(p, str) for p in previous):
        raise ValueError('Invalid managed ownership state')
    owned = set(previous) | set(sources)
    for relative in owned:
        parts = PurePosixPath(relative).parts
        if not parts or (len(parts) > 1 and parts[0] not in ('plugins', 'mods')):
            raise ValueError('Invalid ownership path')
        if len(parts) == 1 and relative != 'server.jar' and Path(relative).suffix.lower() not in ('.yml', '.yaml', '.properties', '.json', '.txt', '.toml', '.conf'):
            raise ValueError('Invalid ownership file')
    for relative in owned:
        destination = safe_path(paths['server'], relative)
        if destination.exists() and not destination.is_file():
            raise ValueError('Managed destination is not a regular file')
    rc = config['rcon']
    password = os.environ.get(rc['password_env'])
    if not password:
        raise ValueError('RCON password environment variable is missing')
    client = RconClient(rc.get('host', '127.0.0.1'), rc.get('port', 25575), password, rc.get('timeout_seconds', 5))
    if client.online() != 0:
        return {'status': 'waiting_for_players', 'commit': commit}
    grace = config['deployment'].get('empty_grace_seconds', 60)
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        time.sleep(min(2, deadline - time.monotonic()))
        if client.online() != 0:
            return {'status': 'waiting_for_players', 'commit': commit}
    if client.online() != 0:
        return {'status': 'waiting_for_players', 'commit': commit}
    service = config['deployment']['service']
    # A player can still join between this check and stop. Grace is not an admission lock.
    client.command('save-all flush')
    systemctl('stop', service)
    backup = paths['backups'] / (commit + '-' + str(time.time_ns()))
    backup.mkdir(parents=True)
    existed = []
    try:
        for relative in sorted(owned):
            destination = safe_path(paths['server'], relative)
            if destination.exists():
                target = safe_path(backup, relative)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(destination, target)
                existed.append(relative)
        atomic_json(backup / 'backup.json', {'commit': state.get('commit'), 'files': existed})
    except Exception:
        systemctl('start', service)
        raise
    try:
        for relative in sorted(owned):
            destination = safe_path(paths['server'], relative)
            if relative in sources:
                copy_atomic(sources[relative], destination)
            else:
                destination.unlink(missing_ok=True)
        systemctl('start', service)
        wait_healthy(client, service, config['deployment'].get('health_timeout_seconds', 120))
    except Exception:
        # Roll back binaries/configs only. Plugin/world data migrations need separate backups.
        systemctl('stop', service)
        for relative in sorted(owned):
            destination = safe_path(paths['server'], relative)
            if relative in existed:
                copy_atomic(safe_path(backup, relative), destination)
            else:
                destination.unlink(missing_ok=True)
        systemctl('start', service)
        raise
    result = {'status': 'deployed', 'commit': commit, 'managed_files': sorted(sources), 'backup': str(backup), 'versions': {a.name: a.version for a in manifest}}
    atomic_json(state_file, result)
    return result


def run(config):
    state = Path(config['paths']['state'])
    state.mkdir(parents=True, exist_ok=True)
    with lock(state / 'deploy.lock'):
        try:
            result = _run(config)
        except Exception as error:
            # Avoid credential-bearing subprocess errors in persistent status.
            atomic_json(state / 'status.json', {'status': 'error', 'error_type': type(error).__name__, 'timestamp': time.time()})
            raise
        atomic_json(state / 'status.json', dict(result, timestamp=time.time()))
        return result
