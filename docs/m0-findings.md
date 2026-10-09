# M0 Spike Findings

Date: 2026-10-09. Environment: Tern (TERM_PROGRAM=tern, pane daemon), yazi 26.9.1 (Terra 2025-12-27), tern-sdk pinned rev 46bd7df. All tests run in a scratch pane via `tern split`/`run`/`send`/`capture`, driven by `scripts/spike_surface.py`.

## S1: Surface coexistence with fullscreen yazi

**Verdict: same-pane inline surface is invisible while yazi is fullscreen. Pivot to split-pane companion (B2).**

| Test | Setup | Result |
|---|---|---|
| Companion foreground, surface alone | `spike_surface.py 8` in pane foreground | ✅ connected (40 kinds, `dock=True`), surface opened, ticks visible in pane capture |
| Companion backgrounded + yazi foreground | `(spike &) && sleep 3 && yazi` | ❌ `connect()` → None. isatty passes for background jobs, so the failure is handshake/reader contention with the shell's ZLE on the shared tty |
| Companion foreground + yazi as child, same tty | `spike_surface.py 120 --with-yazi` | ⚠️ Mixed: frames accepted for 145 ticks, yazi UI intact, **yazi owns keys** (`j` moves highlight), yazi exit detected, clean shutdown — but the inline surface is **not displayed** while yazi holds the screen, and closes with the session after exit |

Conclusions:

1. tern-sdk detection (`available()`) requires tty stdin+stdout, no mux, `TERN_TSP!=0`; `connect()` additionally needs the hello handshake answered — a background process loses the handshake bytes to the foreground shell. **The companion must be the foreground program of its pane.**
2. A fullscreen child (alternate screen) hides the inline transcript where surfaces live. Frames are accepted without error; they just are not shown.
3. Input ownership is safe: a companion that never calls `poll()` does not steal keys from yazi.
4. Open question for M3 (summon panels): can an `overlay`-kind node or a `flow`/`screen`-mode surface show *over* a running program? Untested.

## S2: yazi event/command channels

**Verdict: both channels work. Prefer plugin+DDS for events; `ya emit-to` for commands.**

| Channel | Evidence | Caveats |
|---|---|---|
| `yazi --local-events=hover,cd > file` | ✅ Streams `kind,receiver,sender,{json}` lines; observed `cd` + `hover` with full URLs | Event flood: 10 duplicate `hover` lines for one navigation — reducer must coalesce. Pollution: a child opener writing to stdout (vim) garbles the stream |
| `ya pub-to <id> <custom-kind>` | ❌ Receiver rejects custom kinds: "does not have the ability to receive" (yazi 26.9 ability checks, PR #2696) | Custom kinds need the Lua plugin to declare the ability (`ps.sub`) first — M1 work |
| `ya emit-to <id> cd /tmp` | ✅ yazi in scratch pane navigated to /tmp remotely | Command channel for surface actions confirmed |
| `ya sub <kinds>` | Subscribes to remote-instance messages; untested end-to-end (needs the plugin publishing) | M1 test with `tern.yazi` plugin |

Builtin DDS kinds (docs, v26.9.1): `cd`, `hover`, `rename`, `bulk`, `@yank`, `move`, `trash`, `delete`. Payloads are JSON with `tab` + `url`/`items`/`urls` fields.

## Architecture update (B2)

```
┌─ Tern window ─────────────────────────────────────────────┐
│ pane A: yazi (fullscreen ANSI, pristine)                  │
│  └─ tern.yazi Lua plugin ── ps.pub("tern-*", ...) ──┐     │
│                                                      ▼     │
│ pane B: tern-yazi companion (FOREGROUND, owns tty)  ya sub │
│  ├─ dds.py    event ingestion (ya sub) + `ya emit-to` cmds │
│  ├─ state.py  pure reducer (coalesce floods)               │
│  ├─ views.py  pure view builder                            │
│  └─ app.py    poll loop, inline surface in pane B          │
└────────────────────────────────────────────────────────────┘
```

- Companion splits its own pane (`tern split`/`tern run` from pane B, or launches yazi in pane A via `tern` CLI), keeping both programs' tty ownership clean.
- Companion renders inline surfaces in its own pane — proven working above.
- Events: plugin republishes yazi internals as custom DDS kinds; companion ingests via `ya sub` (no stdout redirection, no opener pollution). Fallback: `--local-events` stdout when companion launches yazi itself.
- Commands: `ya emit-to <yazi-id>` (confirmed).
