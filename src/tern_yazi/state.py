"""Pure reducer over `tern-*` DDS events. No I/O, no tern_sdk imports.

Mirrors hermes-for-tern: every mutation calls `touch()`; the UI renders only
when `revision` advances. Flood control: repeated identical events are dropped
here, not in the view.
"""

from __future__ import annotations

from dataclasses import dataclass, field

KINDS = ("tern-hover", "tern-cd", "tern-ack")


def basename(url: str) -> str:
    return url.rstrip("/").rsplit("/", 1)[-1]


@dataclass
class State:
    cwd: str | None = None
    files: list[str] = field(default_factory=list)
    hovered: str | None = None  # full URL of the hovered entry
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
            self.touch()
        elif kind == "tern-cd":
            url, files = body.get("url"), body.get("files") or []
            if url == self.cwd and files == self.files:
                self.dropped += 1
                return
            self.cwd, self.files = url, list(files)
            self.hovered = None
            self.touch()
        elif kind == "tern-ack":
            self.acks = [*self.acks, body][-5:]
            self.touch()
        else:
            self.dropped += 1
