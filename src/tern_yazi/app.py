"""UI loop: drain DDS events into the reducer, render on revision change (30 Hz cap).

Single-threaded; never blocks — incoming events arrive via `Events.queue`,
outgoing commands go through `Commander` futures. Mirrors hermes-for-tern's App.
"""

from __future__ import annotations

import queue
import time

from tern_sdk import Key

from .dds import Commander, Events
from .state import KINDS, State
from .views import view


class App:
    def __init__(self, session, yazi_id: int):
        self.session = session
        self.state = State()
        self.events = Events(KINDS)
        self.commander = Commander(yazi_id)
        self.exit = False

    def input(self, item) -> None:
        if not isinstance(item, Key):
            return
        if item.name == "q":
            self.exit = True
        elif item.name == "space":
            # Round-trip probe: the plugin answers with a tern-ack broadcast.
            self.commander.publish("tern-cmd", {"op": "ping", "at": time.monotonic()})

    def read_events(self) -> None:
        for _ in range(200):
            try:
                event = self.events.queue.get_nowait()
            except queue.Empty:
                break
            self.state.event(event.kind, event.body)

    def run(self) -> int:
        with (
            self.session,
            self.session.open(mode="inline", title="tern-yazi", role="tern_yazi.panel") as surface,
        ):
            last_render, revision = 0.0, -1
            try:
                while not self.exit and not self.session.closed and not surface.closed:
                    self.read_events()
                    item = self.session.poll(0.02)
                    if item is not None:
                        self.input(item)
                    now = time.monotonic()
                    if self.state.revision != revision and now - last_render >= 0.033:
                        surface.render(view(self.state))
                        revision, last_render = self.state.revision, now
            finally:
                self.events.close()
                self.commander.close()
        return 0
