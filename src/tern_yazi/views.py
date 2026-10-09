"""Pure view builder: State -> Tern semantic tree. No I/O beyond tern_sdk.ui constructors."""

from __future__ import annotations

from tern_sdk import ui

from .state import State

CLIP = 40  # max file rows rendered; yazi pane is the real browser


def file_list(state: State):
    if not state.files:
        return ui.text("waiting for yazi…" if state.cwd is None else "empty directory", key="files-empty")
    hovered = state.hovered_name
    rows = []
    for name in state.files[:CLIP]:
        if name == hovered:
            rows.append(ui.text(f"❯ {name}", tone="accent", key=f"f-{name}"))
        else:
            rows.append(ui.text(f"  {name}", key=f"f-{name}"))
    if len(state.files) > CLIP:
        rows.append(ui.text(f"  … {len(state.files) - CLIP} more", key="files-more"))
    return ui.col(*rows, key="files")


def view(state: State):
    return {
        "main": ui.col(
            ui.row(ui.badge("yazi"), ui.text(state.cwd or "not connected", key="cwd"), key="head"),
            ui.rule(),
            file_list(state),
            ui.status(
                ui.seg("tern-yazi"),
                ui.seg(
                    f"{len(state.files)} files · {state.received} events · rev {state.revision}", side="right"
                ),
                key="bar",
                transparent=True,
            ),
            key="root",
        )
    }
