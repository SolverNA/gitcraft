"""Resolve stable compatible Modrinth releases into reviewed, pinned manifests.

This module only edits a checkout. Publishing the changes is a separate CI step.
"""
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
from urllib.parse import quote, urlencode, urlparse
from urllib.request import HTTPSHandler, HTTPRedirectHandler, Request, build_opener

API = "https://api.modrinth.com/v2"
MAX_METADATA = 16 * 1024 * 1024
MAX_ARTIFACT = 512 * 1024 * 1024


class UpdateError(ValueError):
    pass


def _https(url):
    parsed = urlparse(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        raise UpdateError("Dependency URLs must use HTTPS without embedded credentials")
    return url


class _Redirects(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return super().redirect_request(req, fp, code, msg, headers, _https(newurl))


def _open(url):
    request = Request(_https(url), headers={"User-Agent": "GitCraft/0.1 (Minecraft GitOps)"})
    return build_opener(HTTPSHandler(), _Redirects()).open(request, timeout=30)


def fetch_versions(source):
    for key in ("project", "minecraft", "loader"):
        if not isinstance(source.get(key), str) or not source[key]:
            raise UpdateError(f"Modrinth source requires {key}")
    if not re.fullmatch(r"[A-Za-z0-9_-]+", source["project"]):
        raise UpdateError("Invalid Modrinth project ID or slug")
    if source.get("channel", "release") != "release":
        raise UpdateError("Only stable release updates are supported")
    query = urlencode({"game_versions": json.dumps([source["minecraft"]]),
                       "loaders": json.dumps([source["loader"]]), "include_changelog": "false"})
    with _open(f"{API}/project/{quote(source['project'], safe='')}/version?{query}") as response:
        payload = response.read(MAX_METADATA + 1)
    if len(payload) > MAX_METADATA:
        raise UpdateError("Modrinth response exceeds metadata limit")
    value = json.loads(payload)
    if not isinstance(value, list):
        raise UpdateError("Unexpected Modrinth response")
    return value


def hash_release(file):
    """Compute the local SHA-256 and verify Modrinth's independent SHA-512."""
    expected = file.get("hashes", {}).get("sha512", "")
    if not re.fullmatch(r"[a-fA-F0-9]{128}", expected):
        raise UpdateError("Modrinth file lacks a valid SHA-512")
    declared = file.get("size")
    if type(declared) is not int or not 0 < declared <= MAX_ARTIFACT:
        raise UpdateError("Modrinth artifact size is invalid or exceeds the limit")
    sha256, sha512, size = hashlib.sha256(), hashlib.sha512(), 0
    with _open(file["url"]) as response:
        while True:
            block = response.read(1024 * 1024)
            if not block:
                break
            size += len(block)
            if size > declared or size > MAX_ARTIFACT:
                raise UpdateError("Artifact exceeds declared size")
            sha256.update(block)
            sha512.update(block)
    if size != declared or sha512.hexdigest() != expected.lower():
        raise UpdateError("Downloaded artifact failed size or SHA-512 verification")
    return sha256.hexdigest()


def update_artifact(artifact):
    source = artifact.get("source", {})
    provider = source.get("provider", "direct")
    if provider == "direct":
        return artifact, None
    if provider != "modrinth":
        raise UpdateError(f"Unsupported update provider: {provider}")
    releases = [v for v in fetch_versions(source)
                if v.get("version_type") == "release" and v.get("status") == "listed"
                and source["minecraft"] in v.get("game_versions", [])
                and source["loader"] in v.get("loaders", [])]
    if not releases:
        raise UpdateError(f"No stable compatible release for {artifact['name']}")
    releases.sort(key=lambda v: v["date_published"], reverse=True)
    pinned = artifact["pinned"]
    current = [v for v in releases if v.get("id") == pinned.get("version_id")
               or v.get("version_number") == pinned["version"]]
    if not current:
        raise UpdateError(f"Pinned version for {artifact['name']} is absent from compatible releases; refusing a possible downgrade")
    latest = releases[0]
    if latest["id"] == current[0]["id"]:
        return artifact, None
    files = [f for f in latest.get("files", []) if f.get("filename", "").endswith(".jar")
             and f.get("file_type") not in {"sources-jar", "dev-jar", "javadoc-jar", "signature"}]
    primary = [f for f in files if f.get("primary")]
    if len(primary) == 1:
        file = primary[0]
    elif len(files) == 1:
        file = files[0]
    else:
        raise UpdateError("Release has ambiguous JAR files; pin it manually")
    digest = hash_release(file)
    updated = deepcopy(artifact)
    updated["pinned"] = {"version": latest["version_number"], "version_id": latest["id"],
                         "url": _https(file["url"]), "sha256": digest}
    return updated, {"name": artifact["name"], "from": pinned["version"],
                     "to": latest["version_number"], "sha256": digest}


def update_repository(repository: Path) -> dict:
    """Resolve everything before writing, so resolution failures leave files intact."""
    from .artifacts import load_manifest
    repository = Path(repository)
    load_manifest(repository)  # Reject malformed input before making network requests.
    pending, updates = [], []
    for filename, key in (("engine.json", None), ("plugins.json", "plugins"), ("mods.json", "mods")):
        path = repository / "requirements" / filename
        if not path.exists():
            continue
        value = json.loads(path.read_text(encoding="utf-8"))
        artifacts = [value] if key is None else value[key]
        changed = False
        for index, artifact in enumerate(artifacts):
            replacement, detail = update_artifact(artifact)
            if detail:
                artifacts[index] = replacement
                updates.append(detail)
                changed = True
        if changed:
            pending.append((path, artifacts[0] if key is None else value))
    changed_paths = []
    for path, value in pending:
        fd, temporary = tempfile.mkstemp(prefix=".update-", dir=path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as stream:
                json.dump(value, stream, indent=2, ensure_ascii=False)
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        changed_paths.append(str(path.relative_to(repository)))
    return {"changed": changed_paths, "updates": updates}
