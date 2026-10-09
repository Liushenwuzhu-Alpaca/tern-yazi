"""Environment detection and diagnostics for the Tern companion panel."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys

import tern_sdk


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


def doctor() -> None:
    """Print diagnostics: Tern SDK protocol, detection override, yazi/ya binaries."""
    bypassed = os.environ.get("TERN_TSP") == "0"
    print(f"Tern SDK: {tern_sdk.wire.VERSION} (protocol)")
    print(f"Tern detection: {'disabled via TERN_TSP=0' if bypassed else 'active'}")
    for name in ("yazi", "ya"):
        path, version = bin_info(name)
        print(f"{name}: {path or 'not found'}" + (f" ({version})" if version else ""))


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if not args or args == ["--doctor"]:
        doctor()
        return 0
    print(f"tern-yazi: unknown arguments: {' '.join(args)}", file=sys.stderr)
    print("usage: tern-yazi [--doctor]", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
