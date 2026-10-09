"""DDS transport: yazi events in via `ya sub`, commands out via `ya pub-to`/`ya emit-to`.

Threads live only at this boundary; the UI thread consumes `.queue` and Futures.
"""

from __future__ import annotations

import json
import os
import queue
import signal
import subprocess
import threading
import time
from collections.abc import Sequence
from concurrent.futures import Future, ThreadPoolExecutor
from dataclasses import dataclass

DEBUG = os.environ.get("TERN_YAZI_DEBUG")


def dlog(msg: str) -> None:
    if DEBUG:
        with open(DEBUG, "a") as log:
            log.write(f"{time.strftime('%H:%M:%S')} {msg}\n")


@dataclass
class Event:
    kind: str
    receiver: int
    sender: int
    body: dict


def parse_line(line: str) -> Event | None:
    """Parse one `ya sub` / `--local-events` line: `kind,receiver,sender,{json}`.

    Returns None for polluted lines (openers writing to the same stdout).
    """
    parts = line.rstrip("\n").split(",", 3)
    if len(parts) != 4 or not parts[0]:
        return None
    kind, receiver, sender, raw = parts
    try:
        body = json.loads(raw) if raw else {}
        event = Event(kind, int(receiver), int(sender), body)
    except (ValueError, TypeError):
        return None
    return event if isinstance(event.body, dict) else None


class Events:
    """Subscribes to DDS kinds via `ya sub`; parsed events land in `.queue`."""

    def __init__(self, kinds: Sequence[str], ya: str = "ya"):
        self.queue: queue.Queue[Event] = queue.Queue()
        self.process = subprocess.Popen(
            [ya, "sub", ",".join(kinds)],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            start_new_session=True,
        )
        self._thread = threading.Thread(target=self._reader, daemon=True)
        self._thread.start()

    def _reader(self) -> None:
        assert self.process.stdout
        dlog(f"reader start pid={self.process.pid}")
        try:
            for line in self.process.stdout:
                dlog(f"raw: {line.rstrip()[:120]}")
                event = parse_line(line)
                if event is not None:
                    self.queue.put(event)
        except Exception as exc:
            dlog(f"reader died: {exc!r}")
        dlog("reader exit")

    def close(self) -> None:
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)


class Commander:
    """Sends actions/messages to a yazi instance; every call returns a Future."""

    def __init__(self, yazi_id: int, ya: str = "ya"):
        self.yazi_id = yazi_id
        self._ya = ya
        self._pool = ThreadPoolExecutor(max_workers=1)

    def _run(self, args: list[str]) -> tuple[int, str]:
        out = subprocess.run([self._ya, *args], capture_output=True, text=True, timeout=5)
        return out.returncode, out.stderr.strip()

    def emit(self, *action: str) -> Future:
        """Run a yazi action, e.g. `emit("cd", "/tmp")`."""
        return self._pool.submit(self._run, ["emit-to", str(self.yazi_id), *action])

    def publish(self, kind: str, body: dict) -> Future:
        """Publish a custom kind to the instance's plugins, e.g. `tern-cmd`."""
        return self._pool.submit(self._run, ["pub-to", str(self.yazi_id), kind, "--json", json.dumps(body)])

    def close(self) -> None:
        self._pool.shutdown(wait=False)
