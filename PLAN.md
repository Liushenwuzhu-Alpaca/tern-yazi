# tern-yazi Development Plan

Plan B ("companion panel"): a Tern-native side panel for yazi. yazi stays the fullscreen ANSI TUI; a Python companion process renders native Tern surfaces (hover preview/metadata, selection, task progress, action cards) driven by yazi events, and sends commands back. Zero yazi patches: integration goes only through yazi's Lua plugin API and DDS.

Modeled on `hermes-for-tern`: external adapter process + structured protocol + pure reducer + pure views + single-threaded poll loop.

## Architecture

```
┌─ Tern pane ───────────────────────────────────────────────────┐
│ yazi (fullscreen ANSI TUI)                                    │
│  └─ tern.yazi Lua plugin ── ps.pub_to / --local-events ──┐    │
│                                                           ▼    │
│ tern-yazi (Python companion, tern-sdk)              events     │
│  ├─ dds.py    event ingestion + command channel (`ya emit-to`) │
│  ├─ state.py  pure reducer over yazi events (SDK-free)         │
│  ├─ views.py  pure view builder → Tern semantic tree           │
│  └─ app.py    50 Hz poll loop, 30 Hz render on revision change │
│                           │                                    │
│                           ▼ inline surface (native components) │
│              hover card · selection pill · tasks · actions     │
└───────────────────────────────────────────────────────────────┘
```

Data flow:

1. Companion starts inside Tern (`tern_sdk.connect` detection, passthrough/abort outside), discovers or launches yazi.
2. Lua plugin publishes hover/cd/select/task events over DDS; companion ingests them.
3. Reducer folds events into `State`; `touch()` bumps `revision`; render throttled to 30 Hz.
4. Surface actions (buttons/keys) translate to `ya emit-to <yazi-id> <command>`.

## Components

| Path | Role | Milestone |
|---|---|---|
| `src/tern_yazi/launcher.py` | Tern detection, yazi/ya discovery, `--doctor` | M1 |
| `src/tern_yazi/dds.py` | DDS event ingestion + command channel | M1 |
| `src/tern_yazi/state.py` | Pure reducer (hover, cwd, selection, tasks) | M2 |
| `src/tern_yazi/views.py` | Pure view builder (hover card, selection pill, task list) | M2 |
| `src/tern_yazi/app.py` | UI loop, surface lifecycle, action dispatch | M2 |
| `yazi-plugin/tern.yazi/` | Lua publisher plugin (install via `ya pkg` or symlink) | M1 |

## Milestones

### M0 — Spikes (go/no-go)

- **S1: Surface coexistence.** While yazi runs fullscreen in a Tern pane, a second process connects via tern-sdk and opens an `inline` surface.
  - Acceptance: surface renders and updates while yazi stays interactive.
  - On failure: fall back to summoned panels only (surface opened on demand, closed before returning to yazi), or pivot to `screen`-mode replacement (= plan C).
- **S2: DDS granularity.** Verify which event kinds stream via `yazi --local-events` (hover, cd, select, tasks), that a Lua plugin can publish custom kinds (`ps.pub`/`ps.pub_to`), and that `ya emit-to` drives yazi (reveal, cd, rename).
  - Acceptance: documented list of usable events + command round-trip demo. Record results in `docs/m0-findings.md`.

### M1 — Transport & launcher

- Companion launches yazi as a child with `--local-events <kinds>` (mirrors `hermes-for-tern` Backend: subprocess + reader thread + `queue.Queue`), or attaches to a running instance via DDS.
- Command channel: `ya emit-to` subprocess wrapper with timeouts; never blocks the UI thread.
- Ship `yazi-plugin/tern.yazi/main.lua` publishing custom kinds for what `--local-events` does not cover (check yazi plugin docs; do not guess the API).
- `--doctor` reports: Tern SDK protocol version, yazi/ya paths and versions, DDS reachability.

### M2 — State & views

- Reducer: hovered entry (name, mime, size, mtime), cwd, selection set, task progress.
- Views: hover metadata card, selection pill, task list; 48k-style output clipping where relevant.
- Tests: `tests/test_state.py` (pure reducer, no mocks), `tests/test_views.py` (`View.build` + diff assertions) — same mapping as hermes-for-tern.

### M3 — Action cards

- Destructive-op confirmations surfaced as native cards (bulk rename, trash); answers go back through the command channel.
- Keybinding in yazi (functional plugin) to summon/dismiss the panel.

### M4 — Polish

- `design.py` + `tern-yazi.css` (never restyle measured boxes; honor `.sf-paused`/`.sf-covered`/`.sf-still`/`prefers-reduced-motion`).
- README media via `tern shot` scenario scripts + ffmpeg (`scripts/render_previews.sh`).
- `ruff check`/`format` + full pytest parity with CI.

## Risks

- **Coexistence unknown** (S1) is the project-killer; run it before writing M1 code.
- **DDS is BETA** upstream: pin supported yazi version in README, feature-detect at runtime, degrade gracefully.
- **Event flood** on rapid navigation: coalesce per-kind in the reducer, keep latest only.
- **Pane ownership**: Tern allows one inline transcript; the companion must not fight yazi over focus — surface interactions are keyboard-driven only when summoned (M3).

## Conventions (same as hermes-for-tern)

- uv + committed lockfile; hatchling; src layout; Python ≥ 3.11.
- ruff line-length 110, `target-version = "py311"`; no type checker.
- `state.py` is a pure reducer decoupled from tern-sdk; every mutation calls `touch()`.
- Never block the UI thread: no `sleep`, no blocking `.result()`; threads only at the subprocess boundary.
- Tern edit events use UTF-16 code units — translate, never slice raw.
- Verify before claiming done: `uv run pytest -q && uv run ruff check src tests && uv run ruff format --check src tests`.
