"""Pure view builder: State -> Tern semantic tree. No I/O beyond tern_sdk.ui constructors."""

from __future__ import annotations

import time

from tern_sdk import ui

from .state import State

CLIP = 30  # max file rows rendered; yazi pane is the real browser


def fmt_size(size: int | None) -> str:
    if size is None:
        return "—"
    value = float(size)
    for unit in ("B", "K", "M", "G", "T"):
        if value < 1024 or unit == "T":
            return f"{int(value)}B" if unit == "B" else f"{value:.1f}{unit}".replace(".0", "")
        value /= 1024
    return str(size)


def fmt_mtime(mtime: int | None) -> str:
    return time.strftime("%Y-%m-%d %H:%M", time.localtime(mtime)) if mtime else "—"


def meta_card(state: State):
    if state.hovered is None:
        return None
    kind = "dir" if state.hovered_dir else "file"
    return ui.card(
        ui.kv(
            [
                ("type", kind),
                ("size", fmt_size(state.hovered_size)),
                ("mtime", fmt_mtime(state.hovered_mtime)),
            ]
        ),
        head=state.hovered_name,
        key="meta",
    )


def task_bar(state: State):
    if state.tasks.running <= 0:
        return None
    return ui.row(
        ui.badge("tasks"),
        ui.progress(state.tasks.ratio, label=f"{state.tasks.running} running"),
        key="tasks",
    )


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
            ui.row(
                ui.badge("yazi"),
                ui.text(state.cwd or "not connected", key="cwd"),
                ui.badge(f"{state.selected} selected") if state.selected else None,
                key="head",
            ),
            ui.rule(),
            file_list(state),
            meta_card(state),
            task_bar(state),
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
