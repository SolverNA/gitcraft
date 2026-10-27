import http.client
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch
from urllib.parse import urlencode
from http.server import ThreadingHTTPServer
import yaml
from minecraft_gitops.config import ConfigError, load_config, save_config, validate_config
from minecraft_gitops.web import make_handler


def sample(root):
    return {"repository": {"url": "git@github.com:example/config.git"},
            "paths": {key: str(root / key) for key in ("repository", "server", "state", "cache", "staging", "backups")}}


class ConfigTests(unittest.TestCase):
    def test_reject_invalid_and_overlapping_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            value = sample(Path(temporary))
            value["paths"]["state"] = value["paths"]["server"] + "/state"
            with self.assertRaises(ConfigError):
                validate_config(value)

    def test_secrets_and_public_bind_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            value = sample(Path(temporary))
            value["web"] = {"host": "0.0.0.0"}
            with self.assertRaises(ConfigError):
                validate_config(value)
            value.pop("web")
            value["repository"]["url"] = "https://token@github.com/example/repo"
            with self.assertRaises(ConfigError):
                validate_config(value)

    def test_atomic_save_private_and_roundtrip(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "agent.yml"
            save_config(path, sample(Path(temporary)))
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(load_config(path)["web"]["host"], "127.0.0.1")
            self.assertEqual(list(Path(temporary).glob(".config-*")), [])


class WebTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "agent.yml"
        save_config(self.path, sample(Path(self.temp.name)))
        self.env = patch.dict(os.environ, {"GITCRAFT_WEB_TOKEN": "secret-token-for-testing-123456789"})
        self.env.start()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(self.path))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.env.stop()
        self.temp.cleanup()

    def request(self, path, data=None, cookie=None):
        connection = http.client.HTTPConnection(*self.server.server_address, timeout=5)
        headers = {"Content-Type": "application/x-www-form-urlencoded"}
        if cookie:
            headers["Cookie"] = cookie
        connection.request("POST" if data is not None else "GET", path, urlencode(data) if data is not None else None, headers)
        response = connection.getresponse()
        result = response.status, response.getheaders(), response.read().decode()
        connection.close()
        return result

    def test_login_session_csrf_and_safe_edit(self):
        self.assertEqual(self.request("/deploy", {"csrf": "fake"})[0], 403)
        self.assertEqual(self.request("/login", {"token": "wrong"})[0], 403)
        status, headers, _ = self.request("/login", {"token": os.environ["GITCRAFT_WEB_TOKEN"]})
        self.assertEqual(status, 200)
        cookie = dict(headers)["Set-Cookie"].split(";", 1)[0]
        self.assertIn("HttpOnly", dict(headers)["Set-Cookie"])
        _, _, page = self.request("/", cookie=cookie)
        self.assertNotIn(os.environ["GITCRAFT_WEB_TOKEN"], page)
        import re
        csrf = re.search(r'name="csrf" value="([^"]+)"', page).group(1)
        values = {"poll_interval_seconds": 123, "empty_grace_seconds": 10, "health_timeout_seconds": 90}
        self.assertEqual(self.request("/settings", {"csrf": csrf, "settings": json.dumps(values)}, cookie)[0], 200)
        self.assertEqual(load_config(self.path)["repository"]["poll_interval_seconds"], 123)
        values["shell"] = "arbitrary"
        self.assertEqual(self.request("/settings", {"csrf": csrf, "settings": json.dumps(values)}, cookie)[0], 400)


if __name__ == "__main__":
    unittest.main()
