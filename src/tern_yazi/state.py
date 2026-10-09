"""Pure reducer over `tern-*` DDS events. No I/O, no tern_sdk imports.

Mirrors hermes-for-tern: every mutation calls `touch()`; the UI renders only
when `revision` advances. Flood control: repeated identical events are dropped
here, not in the view.
"""

from __future__ import annotations

from dataclasses import dataclass, field

KINDS = ("tern-hover", "tern-cd", "tern-state", "tern-ack")


def basename(url: str) -> str:
    return url.rstrip("/").rsplit("/", 1)[-1]


@dataclass
class Tasks:
    total: int = 0
    succ: int = 0
    fail: int = 0
    found: int = 0
    processed: int = 0

    @property
    def running(self) -> int:
        return self.total - self.succ - self.fail

    @property
    def ratio(self) -> float | None:
        return self.processed / self.found if self.found else None


@dataclass
class State:
    cwd: str | None = None
    files: list[str] = field(default_factory=list)
    hovered: str | None = None  # full URL of the hovered entry
    hovered_dir: bool = False
    hovered_size: int | None = None
    hovered_mtime: int | None = None
    selected: int = 0
    tasks: Tasks = field(default_factory=Tasks)
    acks: list[dict] = field(default_factory=list)
    received: int = 0
    dropped: int = 0
    revision: int = 0

    @property
    def hovered_name(self) -> str | None:
        return basename(self.hovered) if self.hovered else None

    def touch(self) -> None:
        self.revision += 1

    def event(self, kind: str, body: dict) -> None:
        self.received += 1
        if kind == "tern-hover":
            url = body.get("url")
            if not url or url == self.hovered:
                self.dropped += 1
                return
            self.hovered = url
            self.hovered_dir = bool(body.get("dir"))
            self.hovered_size = body.get("size")
            self.hovered_mtime = body.get("mtime")
            self._select(body.get("selected"))
            self.touch()
        elif kind == "tern-cd":
            url, files = body.get("url"), body.get("files") or []
            if url == self.cwd and files == self.files:
                self.dropped += 1
                return
            self.cwd, self.files = url, list(files)
            self.hovered = None
            self._select(body.get("selected"))
            self.touch()
        elif kind == "tern-state":
            changed = self._select(body.get("selected"))
            tasks = body.get("tasks") or {}
            tasks = Tasks(**{k: tasks.get(k, 0) for k in ("total", "succ", "fail", "found", "processed")})
            if tasks != self.tasks:
                self.tasks = tasks
                changed = True
            if changed:
                self.touch()
            else:
                self.dropped += 1
        elif kind == "tern-ack":
            self.acks = [*self.acks, body][-5:]
            self.touch()
        else:
            self.dropped += 1

    def _select(self, count) -> bool:
        if count is None or count == self.selected:
            return False
        self.selected = count
        return True
