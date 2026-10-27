import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from minecraft_gitops import updater


def artifact():
    return {"name": "Test", "filename": "Test.jar", "pinned": {
        "version": "1", "url": "https://example.org/old.jar", "sha256": "a" * 64},
        "source": {"provider": "modrinth", "project": "test", "minecraft": "1.21", "loader": "paper"}}


def release(version, date, **extra):
    return dict({"id": version, "version_number": version, "date_published": date,
                 "version_type": "release", "status": "listed", "game_versions": ["1.21"],
                 "loaders": ["paper"], "files": [{"filename": "plugin.jar", "primary": True,
                   "url": "https://example.org/plugin.jar", "size": 3,
                   "hashes": {"sha512": hashlib.sha512(b"jar").hexdigest()}}]}, **extra)


class UpdaterTests(unittest.TestCase):
    def test_only_compatible_stable_release_is_selected(self):
        versions = [release("1", "2024-01-01"), release("2", "2024-02-01"),
                    release("3", "2024-03-01", version_type="beta"),
                    release("4", "2024-04-01", loaders=["fabric"])]
        with patch.object(updater, "fetch_versions", return_value=versions), \
             patch.object(updater, "hash_release", return_value="b" * 64):
            updated, change = updater.update_artifact(artifact())
        self.assertEqual(updated["pinned"]["version"], "2")
        self.assertEqual(change["from"], "1")
        self.assertEqual(artifact()["pinned"]["version"], "1")

    def test_unknown_current_release_refuses_possible_downgrade(self):
        with patch.object(updater, "fetch_versions", return_value=[release("0", "2023-01-01")]):
            with self.assertRaisesRegex(updater.UpdateError, "downgrade"):
                updater.update_artifact(artifact())

    def test_ambiguous_jar_release_requires_manual_pin(self):
        latest = release("2", "2024-02-01")
        latest["files"] = [{"filename": "a.jar"}, {"filename": "b.jar"}]
        with patch.object(updater, "fetch_versions", return_value=[release("1", "2024-01-01"), latest]):
            with self.assertRaisesRegex(updater.UpdateError, "ambiguous"):
                updater.update_artifact(artifact())

    def test_hash_checks_provider_digest_and_size(self):
        file = release("1", "2024-01-01")["files"][0]
        with patch.object(updater, "_open", return_value=io.BytesIO(b"jar")):
            self.assertEqual(updater.hash_release(file), hashlib.sha256(b"jar").hexdigest())
        with patch.object(updater, "_open", return_value=io.BytesIO(b"bad")):
            with self.assertRaisesRegex(updater.UpdateError, "verification"):
                updater.hash_release(file)
        with patch.object(updater, "_open", return_value=io.BytesIO(b"toolong")):
            with self.assertRaisesRegex(updater.UpdateError, "size"):
                updater.hash_release(file)

    def test_direct_pin_is_not_changed(self):
        value = artifact()
        value["source"] = {"provider": "direct"}
        with patch.object(updater, "_open", side_effect=AssertionError("must not download")):
            self.assertEqual(updater.update_artifact(value), (value, None))

    def test_resolution_failure_does_not_write_prior_updates(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "requirements").mkdir()
            path = root / "requirements/plugins.json"
            original = json.dumps({"plugins": [artifact(), dict(artifact(), name="Other", filename="Other.jar")]})
            path.write_text(original)
            with patch.object(updater, "update_artifact", side_effect=[(artifact(), {"name": "Test"}), updater.UpdateError("failure")]):
                with self.assertRaises(updater.UpdateError):
                    updater.update_repository(root)
            self.assertEqual(path.read_text(), original)

    def test_manifest_update_preserves_local_filename(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "requirements").mkdir()
            path = root / "requirements/plugins.json"
            path.write_text(json.dumps({"plugins": [artifact()]}))
            value = artifact()
            value["pinned"]["version"] = "2"
            with patch.object(updater, "update_artifact", return_value=(value, {"name": "Test"})):
                result = updater.update_repository(root)
            self.assertEqual(result["changed"], ["requirements/plugins.json"])
            self.assertEqual(json.loads(path.read_text())["plugins"][0]["filename"], "Test.jar")

    def test_http_and_credential_urls_rejected(self):
        for url in ("http://example.org/a.jar", "https://token@example.org/a.jar"):
            with self.assertRaises(updater.UpdateError):
                updater._https(url)
