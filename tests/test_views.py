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


def test_unchanged_state_produces_no_ops():
    state = seeded()
    assert build(state).ops(build(state), "s1") == []


def test_hover_event_produces_ops():
    idle = build(seeded())
    moved = seeded()
    moved.event("tern-hover", {"url": "/tmp/b.md"})
    assert idle.ops(build(moved), "s1") != []
