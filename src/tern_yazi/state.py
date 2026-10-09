"""Pure reducer over `tern-*` DDS events. No I/O, no tern_sdk imports.

Mirrors hermes-for-tern: every mutation calls `touch()`; the UI renders only
when `revision` advances. Flood control: repeated identical hover events are
dropped here, not in the view.
"""

from __future__ import annotations

from dataclasses import dataclass, field

KINDS = ("tern-hover", "tern-cd", "tern-ack")


@dataclass
class State:
    cwd: str | None = None
    file_count: int | None = None
    hovered: str | None = None
    acks: list[dict] = field(default_factory=list)
    received: int = 0
    dropped: int = 0
    revision: int = 0

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
            url, files = body.get("url"), body.get("files")
            if url == self.cwd and files == self.file_count:
                self.dropped += 1
                return
            self.cwd, self.file_count = url, files
            self.hovered = None
            self.touch()
        elif kind == "tern-ack":
            self.acks = [*self.acks, body][-5:]
            self.touch()
        else:
            self.dropped += 1
