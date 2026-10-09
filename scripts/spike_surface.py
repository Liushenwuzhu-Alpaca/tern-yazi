"""M0-S1 spike: does a tern-sdk inline surface coexist with a fullscreen TUI in one pane?

Two modes, run inside a Tern pane:

    uv run python scripts/spike_surface.py 30                # surface alone (foreground)
    uv run python scripts/spike_surface.py 120 --with-yazi   # surface + yazi child on same tty

Milestones append to /tmp/tern-yazi-s1.log (or an explicit second path); watch the pane
itself for the real verdict (surface visible? yazi intact? input ownership?).
"""

import subprocess
import sys
import time
from pathlib import Path

import tern_sdk
from tern_sdk import ui


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    with_yazi = "--with-yazi" in args
    args = [a for a in args if not a.startswith("--")]
    duration = int(args[0]) if args else 60
    log_path = Path(args[1] if len(args) > 1 else "/tmp/tern-yazi-s1.log")

    def note(msg: str) -> None:
        with log_path.open("a") as log:
            log.write(f"{time.strftime('%H:%M:%S')} {msg}\n")

    note(f"connecting pid={__import__('os').getpid()} with_yazi={with_yazi}")
    session = tern_sdk.connect(app="tern-yazi-spike", version="0.1.0", features=())
    if session is None:
        note("FAIL connect() returned None (detection refused in this pane)")
        return 1
    note(f"connected kinds={sorted(session.caps.kinds)} dock={session.caps.has('dock')}")

    with session, session.open(mode="inline", title="S1 spike", role="tern_yazi.spike") as surface:
        note("surface opened")
        yazi = None
        if with_yazi:
            yazi = subprocess.Popen(["yazi"])  # inherits this pane's tty
            note(f"yazi spawned pid={yazi.pid}")
        for tick in range(duration * 2):
            if yazi is not None and yazi.poll() is not None:
                note(f"yazi exited rc={yazi.returncode} at tick {tick}")
                break
            try:
                surface.render(
                    ui.col(
                        ui.row(ui.badge(f"tick {tick}"), ui.kbd("j"), ui.text("yazi should own keys")),
                        ui.text(
                            f"S1 spike alive {time.strftime('%H:%M:%S')} — pid {__import__('os').getpid()}"
                        ),
                    )
                )
            except (OSError, RuntimeError) as exc:
                note(f"FAIL render tick {tick}: {exc}")
                return 1
            if tick % 10 == 0:
                note(f"rendered tick {tick}")
            time.sleep(0.5)
        if yazi is not None and yazi.poll() is None:
            yazi.terminate()
            note("yazi terminated by spike")
    note("closed cleanly")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
