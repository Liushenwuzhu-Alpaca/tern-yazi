"""Environment detection, diagnostics, and launch for the Tern companion panel."""

from __future__ import annotations

import os
import random
import shutil
import subprocess
import sys
from pathlib import Path

import tern_sdk

from . import __version__


def bin_info(name: str) -> tuple[str | None, str | None]:
    """Locate a binary and read the first line of its `--version` output."""
    path = shutil.which(name)
    if path is None:
        return None, None
    try:
        out = subprocess.run([path, "--version"], capture_output=True, text=True, timeout=5)
        version = (out.stdout or out.stderr).splitlines()[0].strip()
    except (OSError, subprocess.TimeoutExpired, IndexError):
        version = None
    return path, version


def yazi_config_home() -> Path:
    env = os.environ.get("YAZI_CONFIG_HOME")
    return Path(env) if env else Path.home() / ".config" / "yazi"


def plugin_status() -> str:
    main = yazi_config_home() / "plugins" / "tern.yazi" / "main.lua"
    if not main.is_file():
        return f"not installed (expected {main}; link yazi-plugin/tern.yazi there)"
    init = yazi_config_home() / "init.lua"
    loaded = init.is_file() and "tern" in init.read_text()
    hint = "setup in init.lua" if loaded else 'NOT loaded: add require("tern"):setup() to init.lua'
    return f"installed at {main}, {hint}"


def doctor() -> None:
    """Print diagnostics: Tern SDK protocol, detection override, yazi/ya binaries, plugin."""
    bypassed = os.environ.get("TERN_TSP") == "0"
    print(f"Tern SDK: {tern_sdk.wire.VERSION} (protocol)")
    print(f"Tern detection: {'disabled via TERN_TSP=0' if bypassed else 'active'}")
    for name in ("yazi", "ya"):
        path, version = bin_info(name)
        print(f"{name}: {path or 'not found'}" + (f" ({version})" if version else ""))
    print(f"Plugin: {plugin_status()}")


def launch_yazi(yazi_id: int, yazi_config: str | None) -> None:
    """Split a pane beside this one running yazi with a known client id."""
    pane = os.environ.get("TERN_PANE")
    if not pane:
        raise RuntimeError("TERN_PANE is not set; cannot split a pane for yazi.")
    command = ["yazi", "--client-id", str(yazi_id)]
    if yazi_config:
        command = ["env", f"YAZI_CONFIG_HOME={yazi_config}", *command]
    subprocess.run(["tern", "split", pane, "right", "--keep-open", "--", *command], check=True, timeout=10)


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if args == ["--doctor"]:
        doctor()
        return 0

    attach, yazi_config = None, None
    while args:
        if args[:1] == ["--attach"] and len(args) >= 2:
            attach, args = int(args[1]), args[2:]
        elif args[:1] == ["--yazi-config"] and len(args) >= 2:
            yazi_config, args = args[1], args[2:]
        else:
            print(f"tern-yazi: unknown arguments: {' '.join(args)}", file=sys.stderr)
            print("usage: tern-yazi [--doctor] [--attach ID] [--yazi-config DIR]", file=sys.stderr)
            return 2

    session = tern_sdk.connect(app="tern-yazi", version=__version__, features=())
    if session is None:
        print("tern-yazi: the companion panel needs Tern. Run it inside Tern.", file=sys.stderr)
        return 1

    if attach is not None:
        yazi_id = attach
    else:
        yazi_id = random.getrandbits(30)
        try:
            launch_yazi(yazi_id, yazi_config)
        except (OSError, RuntimeError, subprocess.CalledProcessError) as exc:
            session.close()
            print(f"tern-yazi: could not launch yazi: {exc}", file=sys.stderr)
            return 1

    print(f"tern-yazi: watching yazi {yazi_id} (q to quit)")

    from .app import App

    return App(session, yazi_id).run()


if __name__ == "__main__":
    raise SystemExit(main())
