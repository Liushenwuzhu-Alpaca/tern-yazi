from tern_yazi.state import State


def test_hover_sets_hovered_once():
    state = State()
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    assert state.hovered == "/tmp/a.txt"
    revision = state.revision
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    assert state.revision == revision
    assert state.dropped == 1


def test_cd_resets_hovered_and_counts_files():
    state = State()
    state.event("tern-hover", {"url": "/tmp/a.txt"})
    state.event("tern-cd", {"url": "/tmp", "files": 3})
    assert (state.cwd, state.file_count, state.hovered) == ("/tmp", 3, None)
    revision = state.revision
    state.event("tern-cd", {"url": "/tmp", "files": 3})
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
