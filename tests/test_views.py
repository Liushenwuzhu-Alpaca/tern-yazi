from tern_sdk.reconcile import View

from tern_yazi.state import State
from tern_yazi.views import view


def build(state):
    return View.build(view(state))


def texts(built):
    return [t.wire()["p"].get("text", "") for t in built.nodes().values() if t.wire()["k"] == "text"]


def seeded():
    state = State()
    state.event("tern-cd", {"url": "/tmp", "files": ["a.txt", "b.md"]})
    return state


def test_idle_view_waits_for_yazi():
    assert "waiting for yazi…" in texts(build(State()))


def test_file_list_marks_hovered():
    state = seeded()
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    shown = texts(build(state))
    assert "❯ a.txt" in shown
    assert "  b.md" in shown


def test_metadata_card_and_selection_pill():
    state = seeded()
    state.event(
        "tern-hover", {"url": "/tmp/a.txt", "dir": False, "size": 2048, "mtime": 1760000000, "selected": 1}
    )
    built = build(state)
    values = [i["v"] for t in built.nodes().values() if t.wire()["k"] == "kv" for i in t.wire()["p"]["items"]]
    assert "2K" in values
    badges = [t.wire()["p"].get("text", "") for t in built.nodes().values() if t.wire()["k"] == "badge"]
    assert "1 selected" in badges


def test_task_bar_shown_while_running():
    state = seeded()
    assert all(t.wire()["k"] != "progress" for t in build(state).nodes().values())
    state.event(
        "tern-state",
        {"selected": 0, "tasks": {"total": 1, "succ": 0, "fail": 0, "found": 10, "processed": 5}},
    )
    kinds = [t.wire()["k"] for t in build(state).nodes().values()]
    assert "progress" in kinds


def test_unchanged_state_produces_no_ops():
    state = seeded()
    assert build(state).ops(build(state), "s1") == []


def test_hover_event_produces_ops():
    idle = build(seeded())
    moved = seeded()
    moved.event("tern-hover", {"url": "/tmp/b.md"})
    assert idle.ops(build(moved), "s1") != []
