# Repository Guidelines

## Project Overview

`tern-yazi` is a native [Tern](https://stencil.so/) companion panel for the [Yazi](https://github.com/sxyazi/yazi) terminal file manager.
It operates via a zero-patch, split-pane architecture: Yazi runs in its own pane as an untouched full-screen ANSI TUI, while the Python companion runs in an adjacent pane, communicating with Yazi via Yazi's DDS (Data Distribution Service) IPC and rendering rich, native, declarative UI components via `tern-sdk`.

---

## Architecture & Data Flow

### Split-Pane B2 Topology
```
┌──────────────────────────────────────┐     ┌──────────────────────────────────────┐
│             Pane A (Yazi)            │     │       Pane B (tern-yazi Companion)   │
│  Full-screen ANSI TUI                │     │  Runs in foreground with Tern TTY    │
│                                      │     │                                      │
│  yazi-plugin/tern.yazi/main.lua      │     │  src/tern_yazi/                      │
│    • Hooks cx on hover, cd           │     │    • dds.Events thread (`ya sub`)    │
│    • Status:children_add hook        │     │    • state.State (pure reducer)      │
│    • ps.sub_remote("tern-cmd", ...)  │     │    • views.py (pure compiler)        │
│    • ps.pub_to(0, "tern-*", payload) │     │    • app.py (50Hz poll, 30Hz render) │
└──────────────────┬───────────────────┘     └──────────────────▲───────────────────┘
                   │                                            │
                   └────── Yazi DDS IPC (`ya sub` / `ya pub`) ──┘
```

- **Pane A (Yazi)**: Launched via `tern split $TERN_PANE right --keep-open -- yazi --client-id <id>` with a random 30-bit client ID.
- **Pane B (Companion)**: Must be the foreground program of its pane to complete the `tern_sdk.connect()` handshake. Hosts an inline Tern surface (`mode="inline"`, `role="tern_yazi.panel"`).

### Data Flow & DDS IPC
1. **Inbound Telemetry (Yazi → Companion)**:
   - `main.lua` intercepts internal Yazi events and broadcasts via `ps.pub_to(0, kind, payload)`.
   - Topics:
     - `tern-hover`: Active cursor URL, `dir`, `size`, `mtime`, and live `selected` count.
     - `tern-cd`: Active `url`, file listing `files`, and `selected` count.
     - `tern-state`: Deduped pulse carrying `selected` count and `tasks` (`total`, `succ`, `fail`, `found`, `processed`).
     - `tern-ack`: Response echo for round-trip probes and pings.
   - `src/tern_yazi/dds.py`: Background daemon thread runs `ya sub <kinds>`, stream-parses CSV lines (`kind,receiver,sender,{json}`), filters stdout pollution, and queues `Event` objects onto `queue.Queue`.
2. **Outbound Control (Companion → Yazi)**:
   - `main.lua` explicitly registers receive capability via `ps.sub_remote("tern-cmd", ...)`.
   - `src/tern_yazi/dds.Commander` dispatches async commands in a 1-worker `ThreadPoolExecutor`:
     - `emit(*action)`: Calls `ya emit-to <id> <action>` (e.g. `cd /path`, `leave`, `arrow`).
     - `publish(kind, body)`: Calls `ya pub-to <id> <kind> --json <payload>` (e.g. `{"op": "hello"}` snapshot request, `{"op": "ping"}`).
3. **State Reduction & UI Rendering**:
   - `src/tern_yazi/state.py`: Pure reducer `State.event(kind, body)`. Coalesces repetitive events and drops no-ops; only bumps `self.revision` via `self.touch()` on actual mutations.
   - `src/tern_yazi/app.py`: UI loop drains the queue at 50Hz (`session.poll(0.02)`), checks `state.revision`, and throttles `surface.render(view(self.state))` to a maximum 30Hz (interval >= 33ms).
   - `src/tern_yazi/views.py`: Pure function compiling `State -> Tern UI Tree` with stable node keys for declarative reconciliation.

---

## Key Directories

- `src/tern_yazi/`: Core Python companion package. Contains the UI loop, reducer, view definitions, DDS client, and CLI launcher.
- `yazi-plugin/tern.yazi/`: Zero-patch Yazi Lua companion plugin (`main.lua`). Captures `cx` state and manages DDS pubsub.
- `tests/`: Offline test suite (`test_state.py`, `test_views.py`, `test_dds.py`, `test_launcher.py`). Zero terminal/daemon dependencies.
- `scripts/`: Development spikes and utility scripts (e.g., `scripts/spike_surface.py` for M0 coexistence verification).
- `docs/`: Architecture decision records and findings (`docs/m0-findings.md`).
- `work/`: Scratch directory for experiments and smoke tests (ignored by git).

---

## Development Commands

All Python workflows use `uv`:

```bash
# Environment sync (creates .venv, installs tern-sdk from Git and editable tern-yazi)
uv sync

# Run test suite
uv run pytest -q

# Run single test module or filter
uv run pytest tests/test_state.py -k test_hover

# Lint checks
uv run ruff check src tests scripts

# Code formatting checks
uv run ruff format --check src tests scripts

# Auto-format code
uv run ruff format src tests scripts

# Full pre-commit check (MUST pass before every commit)
uv run pytest -q && uv run ruff check src tests scripts && uv run ruff format --check src tests scripts

# Diagnostic check (verifies Tern, yazi, ya, and plugin installation)
uv run tern-yazi --doctor

# Launch companion (splits pane and spawns Yazi automatically)
uv run tern-yazi

# Attach companion to an already running Yazi client ID
uv run tern-yazi --attach <CLIENT_ID>

# Run with a sandboxed Yazi configuration
uv run tern-yazi --yazi-config /tmp/yzcfg
```

---

## Code Conventions & Common Patterns

### Python Guidelines
- **Formatting**: Ruff enforced, max line length 110, target version Python 3.11 (`target-version = "py311"`). 4-space indentation.
- **Naming**: `snake_case` for functions/variables, `PascalCase` for classes/dataclasses, `UPPER_CASE` for module constants (`KINDS`, `CLIP`).
- **Typing & Dataclasses**: Use `from __future__ import annotations`. Use `@dataclass` for models (`Tasks`, `State`, `Event`). No mypy/pyright required; keep dynamic typing clean and typed via standard annotations.
- **State Reducer Discipline**:
  - `src/tern_yazi/state.py` MUST remain pure: zero I/O, zero `tern_sdk` imports.
  - Every valid state mutation MUST call `self.touch()` to increment `self.revision`.
  - Always implement flood control: drop identical hover URLs or unchanged directories without calling `touch()` (`self.dropped += 1`).
- **Declarative Views**:
  - `src/tern_yazi/views.py` is a pure compiler mapping `State` to `tern_sdk` UI components (`card`, `table`, `kv`, `badge`, `progress`, `rule`, `status`).
  - Every dynamic element MUST have a stable `key` parameter to allow Tern's virtual DOM reconciliation to diff nodes properly.
  - Truncate long lists (`CLIP = 30`) to avoid layout clipping.
- **Concurrency & Non-blocking Main Loop**:
  - Main loop (`App.run`) is strictly single-threaded and non-blocking. Never perform blocking I/O, sleep, or wait on subprocesses inside the main thread.
  - DDS stream reading runs in a dedicated daemon thread in `dds.Events`.
  - CLI actions dispatched by `Commander` run in a single-worker `ThreadPoolExecutor` and return `Future`.

### Yazi Lua Plugin Conventions (`main.lua`)
- **Safety First (`pcall`)**: All accesses to Yazi's `cx` runtime context MUST be wrapped in `pcall`. If Yazi internals drift across versions, fail gracefully with a single error broadcast instead of crashing the Lua event callback chain.
- **Remote Receive Capability**: In Yazi >=26.9, inbound messages via `ya pub-to` will be rejected unless the plugin registers the kind via `ps.sub_remote("<kind>", callback)` during setup.
- **Task Progress Hooking**: Yazi emits no pubsub events for task progress. Task telemetry MUST hook into `Status:children_add(fn, 1000, Status.RIGHT)`, piggybacking on the status bar's redraws.
- **Yazi 26.9 Tasks API**: Tasks state is read via `cx.tasks.summary` (`total`, `success`, `failed`, `percent`) and `cx.tasks.snaps` (`snap.prog.total_bytes`, `snap.prog.processed_bytes`). Do NOT use `cx.tasks.progress` (does not exist in 26.9).
- **Deduped Broadcasts**: Compute a composite pulse key (`selected|total|succ|fail|found|processed`) and only broadcast `tern-state` when the key changes.

---

## Important Files

- `src/tern_yazi/launcher.py`: CLI entry point. Detects environment, handles `--doctor`, executes `tern split`, and starts the companion.
- `src/tern_yazi/app.py`: UI loop. Orchestrates `session.poll()`, queue draining, 30Hz render throttling, and lifecycle cleanup.
- `src/tern_yazi/dds.py`: IPC transport. Implements `Events` (unbuffered `ya sub` reader thread) and `Commander` (async `ya emit-to` / `ya pub-to`).
- `src/tern_yazi/state.py`: Pure state container and event reducer (`Tasks`, `State`).
- `src/tern_yazi/views.py`: Declarative UI component tree generator.
- `yazi-plugin/tern.yazi/main.lua`: Yazi Lua companion plugin.
- `pyproject.toml`: Project metadata, build configuration (Hatchling), dependencies, and tool settings.
- `PLAN.md`: Canonical engineering design document and milestone tracker (M0 through M4).

---

## Runtime/Tooling Preferences

- **Package Manager**: Use `uv` exclusively. Do not call `pip` directly. Commit changes to `uv.lock`.
- **Python Version**: `>=3.11`.
- **External Binaries**:
  - `tern`: Tern terminal emulator. The companion must run inside a Tern pane, or runtime checks can be bypassed for testing with `TERN_TSP=0`.
  - `yazi` (>=26.9.1): File manager binary.
  - `ya`: Yazi CLI companion tool for DDS IPC.
- **Sandboxed Yazi Testing**:
  - Never pollute or mutate `~/.config/yazi` during tests.
  - Use `/tmp/yzcfg` as a scratch configuration sandbox:
    ```bash
    mkdir -p /tmp/yzcfg/plugins
    ln -sfn /path/to/tern-yazi/yazi-plugin/tern.yazi /tmp/yzcfg/plugins/tern.yazi
    echo 'require("tern"):setup()' > /tmp/yzcfg/init.lua
    YAZI_CONFIG_HOME=/tmp/yzcfg yazi --client-id <id>
    ```

---

## Testing & QA

- **Test Framework**: `pytest>=8,<9`.
- **Running Tests**:
  ```bash
  uv run pytest -q
  ```
- **Test Strategy & Test Doubles**:
  - **No Live Yazi / Daemon Required**: All tests run completely offline and finish in <1s.
  - **DDS Mocking (`tests/test_dds.py`)**: Uses a `fake_ya` helper script generated in `tmp_path` to simulate `ya sub` streams and verify arguments passed to `ya emit-to` / `ya pub-to`.
  - **View Reconciliation Testing (`tests/test_views.py`)**: Tests use `tern_sdk.reconcile.View.build(view(state))` to compile views into semantic node trees, asserting wire properties (`kv`, `badge`, `progress`) and verifying diff operations with `View.ops()`.
  - **Reducer Testing (`tests/test_state.py`)**: Verifies state transitions, revision counters, deduplication, and task calculation purely in memory.
  - **Launcher Testing (`tests/test_launcher.py`)**: Uses pytest's `monkeypatch` and `capsys` to verify binary detection and CLI argument errors.
- **Coverage Expectations**:
  - Every new field in `State` or `Tasks` MUST have test cases in `test_state.py` testing both initial values and deduplication/coalescing.
  - Every new UI widget or badge MUST have assertion tests in `test_views.py`.
  - Any change to DDS command serialization MUST have format assertions in `test_dds.py`.
