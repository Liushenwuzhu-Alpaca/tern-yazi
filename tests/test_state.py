from tern_yazi.state import State


def test_hover_sets_hovered_once():
    state = State()
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    assert state.hovered == "/tmp/a.txt"
    assert state.hovered_name == "a.txt"
    revision = state.revision
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    assert state.revision == revision
    assert state.dropped == 1


def test_cd_carries_files_and_resets_hovered():
    state = State()
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    state.event("tern-cd", {"url": "/tmp", "files": ["a.txt", "b.md"]})
    assert (state.cwd, state.files, state.hovered) == ("/tmp", ["a.txt", "b.md"], None)
    revision = state.revision
    state.event("tern-cd", {"url": "/tmp", "files": ["a.txt", "b.md"]})
    assert state.revision == revision


def test_ack_history_is_capped():
    state = State()
    for i in range(8):
        state.event("tern-ack", {"got": {"n": i}})
    assert len(state.acks) == 5
    assert state.acks[-1] == {"got": {"n": 7}}


def test_unknown_kind_dropped():
    state = State()
    state.event("tern-unknown", {"x": 1})
    assert state.revision == 0
    assert state.dropped == 1
    assert state.received == 1
