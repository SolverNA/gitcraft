"""Validated local configuration. Secrets are environment references only."""
from copy import deepcopy
from pathlib import Path
import ipaddress
import os
import re
import tempfile
import yaml


class ConfigError(ValueError):
    pass


DEFAULTS = {
    "repository": {"branch": "main", "poll_interval_seconds": 60},
    "deployment": {"service": "minecraft.service", "empty_grace_seconds": 60,
                   "health_timeout_seconds": 120},
    "rcon": {"host": "127.0.0.1", "port": 25575,
             "password_env": "MINECRAFT_RCON_PASSWORD", "timeout_seconds": 5},
    "web": {"host": "127.0.0.1", "port": 8765, "token_env": "GITCRAFT_WEB_TOKEN"},
}


def validate_config(value: dict) -> dict:
    if not isinstance(value, dict):
        raise ConfigError("Configuration must be a mapping")
    allowed = set(DEFAULTS) | {"paths"}
    if set(value) - allowed:
        raise ConfigError("Unknown configuration sections")
    result = deepcopy(DEFAULTS)
    path_keys = {"repository", "server", "state", "cache", "staging", "backups"}
    for section, fields in value.items():
        if not isinstance(fields, dict):
            raise ConfigError(f"{section} must be a mapping")
        keys = path_keys if section == "paths" else set(DEFAULTS[section]) | ({"url"} if section == "repository" else set())
        if set(fields) - keys:
            raise ConfigError(f"Unknown settings in {section}")
        result.setdefault(section, {}).update(fields)
    repo = result["repository"]
    if not isinstance(repo.get("url"), str) or not repo["url"] or repo["url"].startswith("-"):
        raise ConfigError("repository.url is required")
    if "://" in repo["url"] and "@" in repo["url"].split("://", 1)[1].split("/", 1)[0]:
        raise ConfigError("Credentials must not appear in repository.url")
    if not isinstance(repo["branch"], str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/-]*", repo["branch"]) or ".." in repo["branch"]:
        raise ConfigError("Invalid repository branch")
    paths = result.get("paths", {})
    if set(paths) != path_keys:
        raise ConfigError("paths requires repository, server, state, cache, staging, backups")
    resolved = []
    for key, path in paths.items():
        if not isinstance(path, str) or not Path(path).is_absolute() or Path(path) == Path("/"):
            raise ConfigError(f"paths.{key} must be an absolute non-root path")
        resolved.append(Path(path).resolve())
    for index, path in enumerate(resolved):
        for other in resolved[index + 1:]:
            if path == other or path in other.parents or other in path.parents:
                raise ConfigError("Managed paths must be separate, non-nested directories")
    for section, key, minimum, maximum in [
        ("repository", "poll_interval_seconds", 1, 86400),
        ("deployment", "empty_grace_seconds", 0, 86400),
        ("deployment", "health_timeout_seconds", 1, 3600),
        ("rcon", "port", 1, 65535), ("rcon", "timeout_seconds", 1, 300),
        ("web", "port", 1, 65535),
    ]:
        number = result[section][key]
        if type(number) is not int or not minimum <= number <= maximum:
            raise ConfigError(f"Invalid {section}.{key}")
    if not isinstance(result["deployment"]["service"], str) or not re.fullmatch(r"[A-Za-z0-9_.@-]+\.service", result["deployment"]["service"]):
        raise ConfigError("Invalid systemd service name")
    for section, key in [("rcon", "password_env"), ("web", "token_env")]:
        if not isinstance(result[section][key], str) or not re.fullmatch(r"[A-Z_][A-Z0-9_]*", result[section][key]):
            raise ConfigError(f"Invalid {section}.{key}")
    for section in ("web", "rcon"):
        try:
            address = ipaddress.ip_address(result[section]["host"])
        except (ValueError, TypeError):
            raise ConfigError(f"{section}.host must be a loopback IP address") from None
        if not address.is_loopback:
            raise ConfigError(f"{section}.host must be a loopback IP address")
    return result


def load_config(path: str | Path) -> dict:
    with Path(path).open(encoding="utf-8") as stream:
        return validate_config(yaml.safe_load(stream))


def save_config(path: str | Path, value: dict) -> dict:
    result = validate_config(value)
    target = Path(path)
    fd, temporary = tempfile.mkstemp(prefix=".config-", dir=target.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            yaml.safe_dump(result, stream, sort_keys=False)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, target)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return result
