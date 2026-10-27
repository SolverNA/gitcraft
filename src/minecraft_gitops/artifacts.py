"""Manifest validation and SHA-256-addressed artifact cache."""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
import urllib.parse
import urllib.request

MAX_ARTIFACT_BYTES = 512 * 1024 * 1024


@dataclass(frozen=True)
class Artifact:
    name: str
    destination: str
    url: str
    sha256: str
    version: str


def validate_url(url):
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password or parsed.fragment:
        raise ValueError('Artifact URL must be HTTPS without credentials or fragment')
    return url


class HTTPSRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        validate_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def load_manifest(repo):
    artifacts = []
    for category in ('engine', 'plugins', 'mods'):
        path = Path(repo) / 'requirements' / (category + '.json')
        if not path.exists():
            continue
        data = json.loads(path.read_text())
        entries = [data] if category == 'engine' else data[category]
        if not isinstance(entries, list):
            raise ValueError('Manifest entries must be a list')
        for entry in entries:
            filename = entry['filename']
            if not isinstance(filename, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*\.jar', filename):
                raise ValueError('Unsafe artifact filename')
            if category == 'engine' and filename != 'server.jar':
                raise ValueError('Engine filename must be server.jar')
            pinned = entry['pinned']
            digest = pinned['sha256']
            if not isinstance(digest, str) or not re.fullmatch('[a-fA-F0-9]{64}', digest):
                raise ValueError('Expected full SHA-256 digest')
            destination = filename if category == 'engine' else category + '/' + filename
            artifacts.append(Artifact(str(entry['name']), destination, validate_url(pinned['url']), digest.lower(), str(pinned['version'])))
    destinations = [a.destination for a in artifacts]
    if len(destinations) != len(set(destinations)):
        raise ValueError('Duplicate artifact destination')
    return artifacts


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def download(artifact, cache, max_bytes=MAX_ARTIFACT_BYTES):
    cache = Path(cache)
    cache.mkdir(parents=True, exist_ok=True)
    target = cache / (artifact.sha256 + '.jar')
    if target.is_file() and not target.is_symlink() and sha256(target) == artifact.sha256:
        return target
    descriptor, temporary = tempfile.mkstemp(prefix='.download-', dir=cache)
    try:
        digest, size = hashlib.sha256(), 0
        opener = urllib.request.build_opener(HTTPSRedirect())
        request = urllib.request.Request(validate_url(artifact.url), headers={'User-Agent': 'minecraft-gitops/0.1'})
        with os.fdopen(descriptor, 'wb') as output, opener.open(request, timeout=30) as response:
            validate_url(response.geturl())
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                if size > max_bytes:
                    raise ValueError('Artifact exceeds size limit')
                digest.update(chunk)
                output.write(chunk)
        if digest.hexdigest() != artifact.sha256:
            raise ValueError('Artifact SHA-256 mismatch')
        os.replace(temporary, target)
        return target
    finally:
        Path(temporary).unlink(missing_ok=True)
