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


def test_hover_carries_metadata_and_selection():
    state = State()
    state.event(
        "tern-hover", {"url": "/tmp/sub", "dir": True, "size": None, "mtime": 1760000000, "selected": 2}
    )
    assert state.hovered_dir is True
    assert state.hovered_mtime == 1760000000
    assert state.selected == 2


def test_state_pulse_updates_tasks_and_selection():
    state = State()
    state.event(
        "tern-state",
        {"selected": 3, "tasks": {"total": 2, "succ": 0, "fail": 0, "found": 100, "processed": 40}},
    )
    assert state.selected == 3
    assert state.tasks.running == 2
    assert state.tasks.ratio == 0.4
    revision = state.revision
    state.event(
        "tern-state",
        {"selected": 3, "tasks": {"total": 2, "succ": 0, "fail": 0, "found": 100, "processed": 40}},
    )
    assert state.revision == revision
    assert state.dropped == 1


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
