import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from minecraft_gitops.artifacts import Artifact, download, load_manifest, validate_url


class ArtifactTests(unittest.TestCase):
    def test_unsafe_url_rejected(self):
        for url in ('http://example.org/a.jar', 'https://user:pass@example.org/a.jar', 'file:///a.jar'):
            with self.assertRaises(ValueError):
                validate_url(url)

    def test_manifest_traversal_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'requirements').mkdir()
            (root / 'requirements/plugins.json').write_text(json.dumps({'plugins': [{'name':'x','filename':'../x.jar','pinned':{}}]}))
            with self.assertRaises(ValueError):
                load_manifest(root)

    def test_bad_hash_never_enters_cache(self):
        class Response:
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def geturl(self): return 'https://example.org/x.jar'
            chunks = iter([b'bad', b''])
            def read(self, size): return next(self.chunks)
        with tempfile.TemporaryDirectory() as directory, patch('minecraft_gitops.artifacts.urllib.request.build_opener') as opener:
            opener.return_value.open.return_value = Response()
            artifact = Artifact('x','plugins/x.jar','https://example.org/x.jar','0'*64,'1')
            with self.assertRaises(ValueError): download(artifact, directory)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_valid_cached_artifact_skips_network(self):
        with tempfile.TemporaryDirectory() as directory:
            digest = hashlib.sha256(b'jar').hexdigest()
            target = Path(directory) / (digest + '.jar')
            target.write_bytes(b'jar')
            artifact = Artifact('x','server.jar','https://example.org/x.jar',digest,'1')
            with patch('minecraft_gitops.artifacts.urllib.request.build_opener') as opener:
                self.assertEqual(download(artifact, directory), target)
                opener.assert_not_called()
