"""Loopback administration UI with token login, sessions and CSRF protection."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from http.cookies import SimpleCookie
from pathlib import Path
import hashlib
import hmac
import html
import json
import os
import secrets
import threading
import time
from urllib.parse import parse_qs
from .config import load_config, save_config


def read_status(config):
    state = Path(config["paths"]["state"])
    result = {"service": config["deployment"]["service"]}
    for name in ("status.json", "installed.json"):
        path = state / name
        if path.is_file():
            try:
                result[name.removesuffix(".json")] = json.loads(path.read_text())
            except (ValueError, OSError):
                result[name.removesuffix(".json")] = {"error": "State unavailable"}
    return result


PAGE = """<!doctype html><html lang="ru"><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>GitCraft</title>
<style>body{font:16px system-ui;background:#111827;color:#e5e7eb;max-width:900px;margin:40px auto;padding:20px}button,input,textarea{font:inherit;padding:10px;margin:6px 0}textarea{width:95%;height:220px}pre{white-space:pre-wrap;background:#1f2937;padding:16px}a{color:#93c5fd}</style>
<h1>GitCraft</h1>{content}</html>"""


def make_handler(config_path):
    sessions = {}
    session_lock = threading.Lock()
    action_lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        server_version = "GitCraft"

        def log_message(self, format, *args):
            # Do not log request bodies, login tokens or arbitrary URL parameters.
            pass

        def send(self, code, content, cookie=None):
            encoded = content.encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("X-Frame-Options", "DENY")
            self.send_header("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'")
            if cookie:
                self.send_header("Set-Cookie", cookie)
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def session(self):
            try:
                cookie = SimpleCookie(self.headers.get("Cookie", ""))
                token = cookie["gitcraft_session"].value
            except (KeyError, ValueError):
                return None
            with session_lock:
                value = sessions.get(token)
                if value and value["expires"] > time.monotonic():
                    return value
                sessions.pop(token, None)
            return None

        def do_GET(self):
            if self.path != "/":
                return self.send(404, "Not found")
            session = self.session()
            if not session:
                return self.send(200, PAGE.format(content='<p>Локальная панель управления. Введите токен из файла секретов.</p><form method="post" action="/login"><input name="token" type="password" required autocomplete="current-password"><button>Войти</button></form>'))
            config = load_config(config_path)
            safe = {"poll_interval_seconds": config["repository"]["poll_interval_seconds"],
                    "empty_grace_seconds": config["deployment"]["empty_grace_seconds"],
                    "health_timeout_seconds": config["deployment"]["health_timeout_seconds"]}
            csrf = html.escape(session["csrf"])
            content = '<h2>Состояние</h2><pre>' + html.escape(json.dumps(read_status(config), ensure_ascii=False, indent=2)) + '</pre>'
            content += f'<form method="post" action="/deploy"><input type="hidden" name="csrf" value="{csrf}"><button>Проверить и применить, когда сервер пуст</button></form>'
            content += '<h2>Локальные настройки</h2><p>Эти же значения можно менять в agent.yml. Конфиги Minecraft меняются через PR.</p>'
            content += f'<form method="post" action="/settings"><input type="hidden" name="csrf" value="{csrf}"><textarea name="settings">{html.escape(json.dumps(safe, indent=2))}</textarea><button>Сохранить</button></form>'
            content += f'<form method="post" action="/logout"><input type="hidden" name="csrf" value="{csrf}"><button>Выйти</button></form>'
            self.send(200, PAGE.format(content=content))

        def do_POST(self):
            # Browser requests from other origins cannot authenticate or mutate state.
            origin = self.headers.get("Origin")
            host = self.headers.get("Host", "")
            if origin and origin != f"http://{host}":
                return self.send(403, "Forbidden origin")
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 16384:
                    return self.send(413, "Invalid request size")
                fields = parse_qs(self.rfile.read(length).decode("utf-8"), strict_parsing=True)
            except (ValueError, UnicodeError):
                return self.send(400, "Invalid request")
            if self.path == "/login":
                configured = os.environ.get(load_config(config_path)["web"]["token_env"], "")
                supplied = fields.get("token", [""])[0]
                if not configured or not hmac.compare_digest(hashlib.sha256(configured.encode()).digest(), hashlib.sha256(supplied.encode()).digest()):
                    return self.send(403, "Invalid login")
                token = secrets.token_urlsafe(32)
                with session_lock:
                    for key in list(sessions):
                        if sessions[key]["expires"] < time.monotonic():
                            del sessions[key]
                    if len(sessions) >= 100:
                        sessions.pop(next(iter(sessions)))
                    sessions[token] = {"csrf": secrets.token_urlsafe(32), "expires": time.monotonic() + 3600}
                return self.send(200, '<a href="/">Открыть панель</a>', f"gitcraft_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=3600")
            session = self.session()
            if not session or not hmac.compare_digest(session["csrf"], fields.get("csrf", [""])[0]):
                return self.send(403, "Invalid session or CSRF token")
            if self.path == "/logout":
                with session_lock:
                    for key, value in list(sessions.items()):
                        if value is session:
                            del sessions[key]
                return self.send(200, '<a href="/">Войти</a>', "gitcraft_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
            if not action_lock.acquire(blocking=False):
                return self.send(409, "Another operation is running")
            try:
                config = load_config(config_path)
                if self.path == "/settings":
                    settings = json.loads(fields.get("settings", [""])[0])
                    if not isinstance(settings, dict) or set(settings) != {"poll_interval_seconds", "empty_grace_seconds", "health_timeout_seconds"}:
                        raise ValueError("Invalid settings")
                    config["repository"]["poll_interval_seconds"] = settings["poll_interval_seconds"]
                    for key in ("empty_grace_seconds", "health_timeout_seconds"):
                        config["deployment"][key] = settings[key]
                    save_config(config_path, config)
                    return self.send(200, '<p>Сохранено.</p><a href="/">Вернуться</a>')
                if self.path == "/deploy":
                    from .deploy import run
                    result = run(config)
                    return self.send(200, '<pre>' + html.escape(json.dumps(result, ensure_ascii=False, indent=2)) + '</pre><a href="/">Вернуться</a>')
                self.send(404, "Not found")
            except Exception:
                self.send(400, "Operation failed. Check configuration and service journal.")
            finally:
                action_lock.release()

    return Handler


def serve(config_path):
    config = load_config(config_path)
    if len(os.environ.get(config["web"]["token_env"], "")) < 24:
        raise ValueError("Web token environment variable requires at least 24 characters")
    address = config["web"]["host"]
    if ":" in address:
        import socket
        class IPv6Server(ThreadingHTTPServer):
            address_family = socket.AF_INET6
        server_class = IPv6Server
    else:
        server_class = ThreadingHTTPServer
    with server_class((address, config["web"]["port"]), make_handler(config_path)) as server:
        server.serve_forever()
