from tern_sdk.reconcile import View

from tern_yazi.state import State
from tern_yazi.views import view


def build(state):
    return View.build(view(state))


def texts(built):
    return [t.wire()["p"].get("text", "") for t in built.nodes().values() if t.wire()["k"] == "text"]


def test_idle_view_waits_for_yazi():
    assert "waiting for yazi…" in texts(build(State()))


def test_hovered_card_shows_entry():
    state = State()
    state.event("tern-cd", {"url": "/tmp", "files": 1})
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    built = build(state)
    assert "/tmp/a.txt" in texts(built)
    heads = [t.wire()["p"].get("head") for t in built.nodes().values() if t.wire()["k"] == "card"]
    assert "a.txt" in heads


def test_unchanged_state_produces_no_ops():
    state = State()
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    assert build(state).ops(build(state), "s1") == []


def test_hover_event_produces_ops():
    idle = build(State())
    state = State()
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    assert idle.ops(build(state), "s1") != []
