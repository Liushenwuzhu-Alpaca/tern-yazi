# tern-yazi Development Plan

## Native companion implementation

The active companion is the Luau File Inbox implementation in `host.luau` and
`window.luau`; the DDS/Python architecture below records the earlier milestones.
`plugin.toml` loads `companion.css` for pane-bounded Miller columns: parent/current/
preview widths are 20/45/35 percent of the width after gaps. Columns share the
remaining main-region height; directory lists and document previews scroll locally.
Raster and SVG previews preserve their aspect ratio within the preview column.

Native list selection uses item node IDs, not row numbers. Single-click selects,
updates the preview and reveals the path in Yazi; a parent/child-column click also
moves the current directory to that entry's containing directory. Double-click enters
a directory or opens the revealed file through Yazi. File rows expose full-path
tooltips. External Yazi cursor changes update the companion selection; task-only
snapshots do not discard pending optimistic input. The window poller accepts the
same 256 KiB snapshot limit as the host, so large directories still refresh.

Verification uses isolated `tern serve` fixtures and a sandboxed Yazi instance,
without switching or capturing the user's virtual desktops. Exercise more than 200
entries (native virtualization), all three column clicks, directory activation,
wide/tall raster and SVG previews, and a compact pane. Inspect both renderer
geometry and Yazi's exported `cwd`/`hovered` acknowledgement.

Direction (2026-10-09): **C' first, B2 as fallback.** C' = hidden yazi core + native Tern frontend: yazi runs in a background tab with the `tern.yazi` plugin exporting full state (`ya.sync` + `cx`) over DDS and receiving commands (`ps.sub_remote`); the companion renders the whole file-manager UI natively in its own pane. B2 keeps yazi visible and the companion as a side panel — same plugin, same transport, smaller UI scope. Decide C' vs B2 after the state channel proves out in M1.

Plan B lineage ("companion panel"): yazi stays the fullscreen ANSI TUI; a Python companion process — foreground in its own pane — renders native Tern surfaces driven by yazi events, and sends commands back. Zero yazi patches: integration goes only through yazi's Lua plugin API and DDS.

Modeled on `hermes-for-tern`: adapter process + structured protocol + pure reducer + pure views + single-threaded poll loop.

## Architecture

M0 settled the layout (see `docs/m0-findings.md`): an inline surface cannot share a pane with fullscreen yazi, and the companion must own its pane's tty. Hence two panes:

```
┌─ Tern window ─────────────────────────────────────────────┐
│ pane A: yazi (fullscreen ANSI, pristine)                  │
│  └─ tern.yazi Lua plugin ── ps.pub("tern-*", ...) ──┐     │
│                                                      ▼     │
│ pane B: tern-yazi companion (FOREGROUND, owns tty)  ya sub │
│  ├─ dds.py    event ingestion (ya sub) + `ya emit-to` cmds │
│  ├─ state.py  pure reducer over yazi events (SDK-free)     │
│  ├─ views.py  pure view builder → Tern semantic tree       │
│  └─ app.py    50 Hz poll loop, 30 Hz render on revision    │
│                           │                                │
│                           ▼ inline surface in pane B       │
│              hover card · selection pill · tasks · actions │
└────────────────────────────────────────────────────────────┘
```

Data flow:

1. Companion starts inside Tern (`tern_sdk.connect`; aborts outside), splits/launches yazi in an adjacent pane with a known `--client-id`.
2. Lua plugin republishes yazi internals (hover/cd/selection/tasks) as custom DDS kinds; companion ingests via `ya sub` (fallback: `yazi --local-events` stdout when the companion launches yazi itself).
3. Reducer folds events into `State` (coalescing floods — hover events repeat heavily); `touch()` bumps `revision`; render throttled to 30 Hz.
4. Surface actions (buttons/keys) translate to `ya emit-to <yazi-id> <command>` (confirmed working in M0).

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

### M0 — Spikes (DONE 2026-10-09, see `docs/m0-findings.md`)

- **S1: Surface coexistence.** Same-pane inline surface is invisible while yazi is fullscreen; companion must be foreground in its own pane → split-pane (B2) layout. Open question parked for M3: overlay-kind/`flow`-mode surfaces over a running program (summon panels).
- **S2: DDS granularity.** `--local-events` streams `kind,receiver,sender,{json}` (flood-heavy, coalesce in reducer); custom kinds need plugin-declared abilities (yazi 26.9); `ya emit-to` drives yazi remotely (confirmed `cd /tmp`).

### M1 — Transport & launcher

- Launcher: companion takes pane foreground, splits a pane for yazi via the `tern` CLI (`tern split ... -- yazi --client-id <id>`), or attaches to an existing instance.
- Event channel (primary): `yazi-plugin/tern.yazi/main.lua` subscribes to local events (`ps.sub`) and republishes them as custom `tern-*` kinds, declaring the abilities yazi 26.9 requires; companion ingests via `ya sub tern-hover,tern-cd,...` (reader thread + `queue.Queue`, mirrors `hermes-for-tern` Backend). **Round-trip proven 2026-10-09**: `ps.sub("hover")` + `cx` read → `ps.pub_to(0, ...)` → `ya sub` receives; `ya pub-to <id> tern-cmd` → `ps.sub_remote` receives → broadcast ack received. Known caveat: the hover callback can observe the pre-update `cx` (three rapid `j` moves reported the same URL) — defer the read or verify sequencing in M2.
- Event channel (fallback): when the companion launches yazi itself, `yazi --local-events=hover,cd` stdout parsing; tolerate polluted lines (openers writing to stdout).
- Command channel: `ya emit-to <id>` subprocess wrapper with timeouts; never blocks the UI thread.
- `--doctor` additionally reports DDS reachability (round-trip a builtin kind against a running instance).

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

- ~~Coexistence unknown~~ — resolved in M0: split-pane layout; summon/overlay model untested (M3).
- **DDS is BETA** upstream: pin supported yazi version (developed against 26.9.1) in README, feature-detect at runtime, degrade gracefully. yazi 26.9 enforces receiver abilities for custom kinds.
- **Event flood** on rapid navigation: coalesce per-kind in the reducer, keep latest only.
- **Pane ownership**: Tern allows one inline transcript; the companion must not fight yazi over focus — surface interactions are keyboard-driven only when summoned (M3).

## Conventions (same as hermes-for-tern)

- uv + committed lockfile; hatchling; src layout; Python ≥ 3.11.
- ruff line-length 110, `target-version = "py311"`; no type checker.
- `state.py` is a pure reducer decoupled from tern-sdk; every mutation calls `touch()`.
- Never block the UI thread: no `sleep`, no blocking `.result()`; threads only at the subprocess boundary.
- Tern edit events use UTF-16 code units — translate, never slice raw.
- Verify native changes with `./tests/smoke_luau.sh` plus the isolated renderer/Yazi scenarios above. The Python commands from earlier milestones are historical: this checkout currently has no Python tests or `scripts/` directory.
