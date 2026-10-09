import json
import queue
import stat

import pytest

from tern_yazi.dds import Commander, Events, parse_line


def test_parse_line_valid():
    event = parse_line('tern-hover,0,434343,{"url":"/tmp/a.txt"}\n')
    assert event is not None
    assert (event.kind, event.receiver, event.sender) == ("tern-hover", 0, 434343)
    assert event.body == {"url": "/tmp/a.txt"}


def test_parse_line_tolerates_pollution():
    # Openers sharing stdout can inject garbage (observed: vim writing into the log).
    assert parse_line("Vim: Warning: Output is not to a terminal") is None
    assert parse_line("tern-hover,0,notanumber,{}") is None
    assert parse_line('tern-hover,0,1,["not", "a", "dict"]') is None
    assert parse_line("") is None


def fake_ya(tmp_path, script: str) -> str:
    ya = tmp_path / "ya"
    ya.write_text(f"#!/bin/sh\n{script}\n")
    ya.chmod(ya.stat().st_mode | stat.S_IEXEC)
    return str(ya)


def test_events_reads_parsed_stream(tmp_path):
    ya = fake_ya(
        tmp_path,
        'printf \'%s\\n\' \'tern-cd,0,1,{"url":"/tmp","files":3}\' "garbage line" \'tern-hover,0,1,{"url":"/tmp/a"}\'',
    )
    events = Events(["tern-cd", "tern-hover"], ya=ya)
    try:
        first = events.queue.get(timeout=5)
        second = events.queue.get(timeout=5)
        assert (first.kind, first.body["url"]) == ("tern-cd", "/tmp")
        assert (second.kind, second.body["url"]) == ("tern-hover", "/tmp/a")
        with pytest.raises(queue.Empty):
            events.queue.get(timeout=0.2)
    finally:
        events.close()


def test_commander_emit_and_publish_args(tmp_path):
    log = tmp_path / "args.log"
    ya = fake_ya(tmp_path, f'echo "$@" >> {log}')
    commander = Commander(434343, ya=ya)
    try:
        assert commander.emit("cd", "/tmp").result(timeout=5)[0] == 0
        assert commander.publish("tern-cmd", {"op": "ping"}).result(timeout=5)[0] == 0
    finally:
        commander.close()
    lines = log.read_text().splitlines()
    assert lines[0] == "emit-to 434343 cd /tmp"
    assert lines[1] == f"pub-to 434343 tern-cmd --json {json.dumps({'op': 'ping'})}"
