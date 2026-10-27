"""Command-line entry points shared by the installer and systemd."""
import argparse
import json
import sys
import time
from pathlib import Path
from .config import load_config


def main(argv=None):
    parser = argparse.ArgumentParser(prog="gitcraft")
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("agent", "web", "status"):
        command = commands.add_parser(name)
        command.add_argument("--config", required=True)
        if name == "agent":
            command.add_argument("--once", action="store_true")
    for name in ("validate", "update"):
        command = commands.add_parser(name)
        command.add_argument("--repository", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "validate":
            from .artifacts import load_manifest
            load_manifest(args.repository)
            import yaml
            for directory in ("server-settings", "plugins-configs"):
                for path in (args.repository / directory).rglob("*"):
                    if path.suffix in {".yml", ".yaml"}:
                        with path.open(encoding="utf-8") as stream:
                            yaml.safe_load(stream)
            print(json.dumps({"valid": True}))
        elif args.command == "update":
            from .updater import update_repository
            print(json.dumps(update_repository(args.repository)))
        else:
            config = load_config(args.config)
            if args.command == "web":
                from .web import serve
                serve(Path(args.config))
            elif args.command == "status":
                from .web import read_status
                print(json.dumps(read_status(config)))
            else:
                from .deploy import run
                while True:
                    print(json.dumps(run(config)), flush=True)
                    if args.once:
                        break
                    time.sleep(config["repository"]["poll_interval_seconds"])
                    config = load_config(args.config)
        return 0
    except (Exception, KeyboardInterrupt) as error:
        # Library errors must avoid including credentials in messages.
        print(f"gitcraft: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
