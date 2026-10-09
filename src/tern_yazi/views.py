"""Pure view builder: State -> Tern semantic tree. No I/O beyond tern_sdk.ui constructors."""

from __future__ import annotations

from tern_sdk import ui

from .state import State


def hovered_card(state: State):
    if state.hovered is None:
        body = ui.text("Move in yazi to see the hovered entry here.")
        head = "nothing hovered"
    else:
        body = ui.text(state.hovered, wrap="word")
        head = state.hovered.rstrip("/").rsplit("/", 1)[-1]
    return ui.card(body, head=head, key="hovered")


def view(state: State):
    return {
        "main": ui.col(
            ui.row(ui.badge("yazi"), ui.text(state.cwd or "waiting for yazi…", key="cwd"), key="head"),
            ui.rule(),
            hovered_card(state),
            ui.kv(
                [
                    ("files", str(state.file_count) if state.file_count is not None else "—"),
                    ("events", str(state.received)),
                    ("dropped", str(state.dropped)),
                ],
                key="stats",
            ),
            ui.status(
                ui.seg("tern-yazi"),
                ui.seg(f"rev {state.revision}", side="right"),
                key="bar",
                transparent=True,
            ),
            key="root",
        )
    }
