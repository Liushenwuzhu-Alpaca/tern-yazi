# Development Guide

This document describes the native `tern-yazi` implementation: a Rust PTY supervisor, Tern Luau block/window plugins, and a Yazi Lua plugin. The installed `yazi` wrapper runs the original executable directly outside Tern. Inside a Tern pane it starts Yazi in an invisible PTY and opens a managed visual zoom overlay that preserves the owner shell and original split tree. Yazi owns file-manager state, directory navigation, selection, tasks, and shell execution; Tern owns layout, file opening (`cx:open`), rich previews, input bars, and confirmation. Runtime execution uses the native helper, Tern, Yazi, and Ya, with no Python process.

## 1. Environment and installation

### Runtime reference

The recorded Linux interactive verification environment uses:

| Component | Observed version | Responsibility |
| --- | --- | --- |
| Tern | `0.7.0` (`9ca00e4`) | Host/window plugin VMs, native block UI, file opening (`cx:open`), rich file previews, layout, input routing |
| Yazi | `26.9.1` | Authoritative directory navigation, cursor, selection, visual marks, tasks, and shell execution |
| Ya | `26.9.1` | `ya emit-to` dispatch to the chosen Yazi client |

These are observed versions, not a compatibility matrix. The generated declarations in [`tern.d.luau`](tern.d.luau) describe the Tern APIs used by this checkout. Both Yazi and the Tern host must run on the same machine and see the same runtime inbox. Keep `ya` and `tern` on the host process's `PATH`; Terminal Here invokes `tern` as a subprocess.

Rust and Cargo build the launcher; its locked dependencies are `libc` and `serde_json`. The installer produces a standalone native executable, so Cargo is a build-time requirement rather than a runtime service. A Nerd Font supplies file-type glyphs. Archive paths additionally use installed `unzip`, `tar`, or `7z` executables; listing support and extraction support differ by format.

### Link the checkout

Set `REPO` to the absolute location of a local checkout:

```sh
REPO=/absolute/path/to/tern-yazi
tern plugin link "$REPO"
tern plugin list
```

The manifest identifies the plugin as `tern-yazi`, version `0.2.0`, and the companion block as `tern-yazi.companion`. Linking retains a reference to the checkout; keep it at that path.

Install the Yazi side in your chosen Yazi configuration. For the default Linux configuration:

```sh
mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$REPO/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"
```

Inspect an existing destination before linking. Add the following line to the existing Yazi `init.lua`, preserving any other configuration:

```lua
require("tern"):setup()
```

Start or restart Yazi after changing its configuration. Native Tern source changes are loaded with:

```sh
tern plugin reload
```

Unlink the Tern registration with `tern plugin unlink tern-yazi`. Remove only the Yazi link and setup line you added when uninstalling that half.

### Build and install the local launcher

After linking both plugins and enabling the Yazi setup line, install the owned wrapper:

```sh
sh "$REPO/install.sh" --real /usr/bin/yazi
export PATH="$HOME/.local/bin:$PATH"
yazi
```

`--real` names the existing Yazi executable. Replace `/usr/bin/yazi` with its actual path when needed. The default prefix is `$HOME/.local`; `--prefix /absolute/prefix` changes it. Put that prefix's `bin` before the original Yazi directory on `PATH` and retain that ordering in the shell configuration. When `--real` is omitted, the installer searches `PATH` for an original executable, skipping its own wrapper; an owned reinstall reuses the recorded path.

The installer runs `cargo build --locked --release --manifest-path "$REPO/launcher/Cargo.toml" --target-dir "$REPO/launcher/target"`. It installs `prefix/bin/yazi`, `prefix/libexec/tern-yazi-launch`, the original-path data file `tern-yazi-launch.real`, and the SHA-256 ownership manifest `tern-yazi-launch.owned`. The original executable is retained at its original location. Existing unrelated or modified destination files cause an ownership error rather than replacement. Re-run the same installation command after Rust source changes; `tern plugin reload` loads Luau changes but does not rebuild an installed executable. `/launcher/target/` is ignored by Git; `launcher/Cargo.lock` belongs to the build source.

The helper's internal invocation is `tern-yazi-launch --real ORIGINAL -- [YAZI_ARGS...]`. The wrapper forwards all original arguments. An empty `TERN_PANE` executes original Yazi directly; managed startup requires a positive numeric pane ID. `--help`, `-h`, `--version`, and `-V` before the Yazi argument delimiter execute the original immediately. Managed startup supplies a random client ID and rejects user `--client-id` or `--client-id=...` options before that delimiter. Use the original executable for an explicit-ID ordinary Yazi session. The helper validates the original executable to prevent recursion.

Remove the launcher using the installation prefix:

```sh
sh "$REPO/install.sh" --uninstall
# For a custom prefix:
sh "$REPO/install.sh" --prefix /absolute/prefix --uninstall
```

Uninstall validates the owned files against the manifest and removes only those installation files. It preserves original Yazi and the separate Tern/Yazi plugin registrations. Modified owned files must be inspected before removal; the installer reports the affected path.


### Editor tooling

Regenerate the installed Tern API declarations with:

```sh
tern plugin types .
```

Configure `luau-lsp.types.definitionFiles` to include the resulting `tern.d.luau`. These declarations distinguish host-only and window-only APIs; Yazi's `main.lua` uses Yazi's Lua API, not `tern`.

## 2. Source map

| File | Responsibility |
| --- | --- |
| [`plugin.toml`](plugin.toml) | Plugin identity, host/window entry points, stylesheet, palette-visible companion block |
| [`host.luau`](host.luau) | Thin host entry: requires `frontend/host` and calls `register()` |
| [`window.luau`](window.luau) | Thin window entry: requires `frontend/window` and calls `register()` |
| [`frontend/host.luau`](frontend/host.luau) | Block lifecycle/presentation coordinator; owns native effect contexts and applies backend results to the block |
| [`frontend/view.luau`](frontend/view.luau) | Native UI tree, stable node IDs/keys, regions, and preview presentation |
| [`frontend/format.luau`](frontend/format.luau) | Presentation-only `fmt_size` labels and `file_icon` glyph/tone policy, using backend semantic file classification |
| [`frontend/input.luau`](frontend/input.luau) | Native key/event handling and local input editing, including UTF-16 caret semantics |
| [`frontend/window.luau`](frontend/window.luau) | Window presentation coordinator: native layouts, pane focus/zoom, routes, and terminal/block presentation |
| [`backend/session.luau`](backend/session.luau) | Pinned snapshots, identity, health/lease polling, liveness, and session lifecycle data |
| [`backend/actions.luau`](backend/actions.luau) | Serialized Yazi actor requests, process dispatch, replies, and action completion data |
| [`backend/model.luau`](backend/model.luau) | Pure snapshot types, ordered listing/selection projection, and semantic extension/language/MIME/archive classification; no display labels, icons, tones, or native UI construction |
| [`backend/preview.luau`](backend/preview.luau) | Preview settings, bounded file reads, archive listing data, and data-only preview results |
| [`backend/preview_policy.luau`](backend/preview_policy.luau) | Pure shared byte-limit map and defaults for preview reads and the window settings command |
| [`backend/window.luau`](backend/window.luau) | Managed record/lease reconciliation, reload identity, shell handoff, and window lifecycle bookkeeping |
| [`backend/inbox.luau`](backend/inbox.luau) | Shared runtime inbox resolution, capped JSON reads, and lease-hint parsing |
| [`shared/path.luau`](shared/path.luau) | Pure `basename`, `safe_cid`, and `join_path` helpers; no runtime state or I/O |
| [`companion.css`](companion.css) | Three-column geometry, pane-contained list/preview scrolling, image containment, footer and confirmation styling |
| [`yazi-plugin/tern.yazi/main.lua`](yazi-plugin/tern.yazi/main.lua) | Yazi telemetry hooks, atomic snapshots/replies, request decoding, actor dispatch, target selection guard |
| [`launcher/src/main.rs`](launcher/src/main.rs) | Invisible PTY, generated client/token, startup checks, heartbeat/lease/stop handling, process-group termination and owned runtime cleanup |
| [`launcher/Cargo.toml`](launcher/Cargo.toml), [`launcher/Cargo.lock`](launcher/Cargo.lock) | Locked native launcher build and dependencies |
| [`install.sh`](install.sh) | Local owned wrapper/helper installation, original executable resolution, reversible uninstall |
| [`tern.d.luau`](tern.d.luau) | Generated native Tern type and API contracts |
| [`tests/smoke_luau.sh`](tests/smoke_luau.sh), [`launcher/tests/interaction.rs`](launcher/tests/interaction.rs) | Real isolated Tern/Yazi interaction suite, native state/file outcomes, screenshots and process cleanup |

There is no supported Python entry point or Python test suite in this checkout. Local bytecode or ignored development artifacts are not part of the native runtime. The root `DEVELOPMENT.md` is the published developer guide; the intentionally excluded `/AGENTS.md`, `/PLAN.md`, and `/docs/` remain local and are not installation prerequisites.

## 3. Architecture and lifecycle

```text
shell: yazi [args]
    |
    +-- outside Tern --> original Yazi [args]
    |
    +-- inside Tern --> tern-yazi-launch --real ORIGINAL -- [args]
                          |
                          +-- invisible PTY --> original Yazi --client-id CID [args]
                          |                          |
                          |                 tern.yazi/main.lua (authoritative actor)
                          |                          |
                          |                 atomic state-CID.json / replies
                          |
                          +-- managed-CID.json heartbeat every 500 ms
                                       |
                          backend/window + backend/session
                          (health, snapshots, leases, identity)
                                       |
                   frontend/window             frontend/host
                   (layout/pane effects)        (block presentation coordinator)
                                       |                    |
                             managed visual zoom       frontend/view + input
                             preserving owner shell    (native UI/input)
                                                            |
                                                  backend/actions
                                                  (serialized ya emit-to)
                                                            |
                                                  Yazi actor --> reply + snapshot

backend/session + backend/window --> token-bound lease / stop
                                 --> supervisor --> terminate + reap
backend/preview --> plain preview data --> frontend native preview/blob adapter

host.luau / window.luau: explicit registrations only
frontend --> backend --> shared/path; no reverse UI dependency
```

### Yazi setup and snapshot production

`M:setup()` creates the inbox directory, subscribes to Yazi's local `hover` and `cd` events, registers the remote `tern-cmd` topic, and installs a `Status:children_add(..., 1000, Status.RIGHT)` callback. Local event bodies contain a tab reference rather than a complete state; the plugin reads the settled `cx` state directly in synchronous callbacks.

Hover and directory callbacks publish DDS telemetry, pulse selection/tasks, and call `dump_state()`. Status redraws provide the missing task-progress trigger and also call `dump_state()`. Task totals come from `cx.tasks.summary`; byte counters are summed from `cx.tasks.snaps[*].prog`. The plugin does not use `cx.tasks.progress`.

Snapshot deduplication covers cwd, current filenames, hovered URL, selection count and paths, visual mode and marked paths, and task counters. An unchanged composite key does not write another file. Metadata such as size/mtime and the parent listing are included in snapshots but are not independent deduplication triggers. A metadata-only update therefore need not immediately produce a new snapshot. An explicit request's `snapshot()` clears the deduplication key before dumping.

Writes use `state-<client-id>.tmp`, close the file, and rename it to `state-<client-id>.json`. Readers never intentionally consume the intermediate filename. `seq` increments per emitted snapshot; `ts` uses `ya.time()` when available, otherwise `os.time()`.

### Managed startup and supervisor

The launcher allocates a random positive 30-bit client ID and a session token in the runtime inbox, reserving the ID with an exclusive lock. It uses `forkpty` to provide the original Yazi a controlling terminal and process group. The child receives the generated `--client-id`, followed by the preserved user arguments; Tern pane/window identity variables are removed from the child environment. The supervisor uses a private umask for control files and restores the caller's umask in Yazi, including its file operations and shell children. Yazi's ANSI output stays on the invisible PTY. The supervisor drains bounded chunks and answers basic terminal queries instead of exposing a second terminal pane. Initial PTY dimensions come from the launching terminal when available; token-bound leases subsequently carry the native block's rows and columns.

Startup waits up to 15 seconds for a valid `state-CID.json` with the matching ID and cwd. Only then does it publish the first `managed-CID.json`; thus an incomplete Yazi/plugin startup does not create a managed native pane. It reports an early child exit or missing initial snapshot with a bounded, terminal-control-stripped startup diagnostic. Install and enable the Yazi plugin before launching. The supervisor then allows 15 seconds for the native window to attach. After a lease is observed, an unchanged lease for 8 seconds terminates the backend. These managed deadlines are separate from the ordinary attach probe described below.

The supervisor polls the PTY with a 25 ms timeout and publishes a sequenced health record every 500 ms. Health is independent of snapshot deduplication, so an idle directory remains live without file-manager changes. A stop file containing the exact session token, supervisor signal, child exit, or expired attachment lease ends the session. Shutdown sends SIGTERM to the process group, allows 1.5 seconds, then sends SIGKILL and reaps the child. Linux subreaper handling also collects orphaned descendants with a bounded reap phase. The helper removes its managed health/control/lock files, matching state files, and session-prefixed replies on teardown.

### Window lifecycle and reload adoption

The host records the pane's `EffectCx` from Tern's `command_started` shell-integration event. Its 500 ms discovery timer requires a progressing managed health sequence, then opens that health-record path through the owning pane's effect context. The window's `tern.route.open` claims only `managed-<numeric-client-id>.json` under the shared inbox and calls `poke()` to create the native block. This explicit host effect wakes an idle window; startup does not depend solely on window render timers. `window_start` and the self-rearming 500 ms window timer also reconcile existing managed panes and configuration. When launching a managed overlay, the window focuses the owner pane, records its current zoom state in `tern.kv` under `managed_overlays` (`{ owner_pane, was_zoomed }`), opens the companion block beside the owner via `cx:new_block(..., "beside", {focus=true})` passing client/token/owner/cwd and the startup nonce, and immediately zooms the new block. The visual zoom overlay preserves the owner shell and original split tree underneath.

Each client has its own managed record and pane. Window acknowledgement nonces combine a Linux kernel UUID from `/proc/sys/kernel/random/uuid` with a local counter; window clocks can restart at zero and are not unique epochs. A new block receives its nonce in launch arguments. The block accepts `managed_poll` only for its client/token and a valid nonce, then writes `TOKEN PANE ROWS COLS NONCE` to its lease. The window verifies that exact acknowledgement. A block-owned 500 ms timer retains the verified nonce and refreshes the matching lease independently of window frames; it also polls its pinned snapshot/liveness and renders relevant changes. After reload the window treats the saved lease pane as a hint, not proof: the block must acknowledge the new nonce within 3 seconds, or the window launches a fresh pane.

Closing a managed block or its owner pane writes the token-bound stop request, restores the owner pane's prior zoom state via `restore_overlay()`, cleans the `managed_overlays` KV entry, and clears the matching lease. `q` returns focus directly to the original shell in its prior zoom state. The host timer also checks that both pane IDs still exist in the daemon session. Closed client/token pairs are suppressed while the window VM remains active; the stop/lease state preserves the close decision across reload. Closing a Tern window alone follows Tern's persisted daemon-session semantics: the block and owner panes retain the managed backend, and the block-owned lease timer continues independently of window rendering. Loss of the daemon/host lease eventually triggers the supervisor's 8-second lease deadline. If the supervisor disappears, a surviving block becomes an explicit offline UI rather than being reassigned to a different client. A healthy companion acknowledges its client ID before enabling operations.

`Ctrl+Alt+Y` and `Yazi: Toggle Companion Panel` (`plugin.tern-yazi.toggle`) remain explicit attach/layout controls. When invoked while a managed overlay or its owner pane is focused, toggle focuses and zooms the managed overlay. For an ordinary attached companion, toggle switches between floating and docked layouts. The plugin uses its own shortcut registration; installation does not rewrite persistent global keybindings.
### Block lifecycle and ordinary attach

`tern.block.define("companion", ...)` supplies `init`, `view`, `title`, `event`, `key`, and `save`. The saved state contains `cid`, `token`, `owner_pane`, `source_cwd`, and `lease_nonce`; command/filter editing, optimistic cursor/cwd, requests, confirmation, and archive cache remain transient. A managed block reads only its pinned `state-CID.json` and requires observed heartbeat progression before enabling file actions. Unreadable/invalid snapshots are skipped, and all JSON reads have a 256 KiB cap; very large listings/selections can exceed that cap.

The host timer reads the latest `BlockCx` retained by `view`, rather than keeping the initialization context's default 24-by-80 dimensions. Resize renders refresh that context before the next lease update, keeping the invisible PTY aligned with the native pane.

An explicit ordinary attach initially chooses the highest timestamp/sequence score among numeric `state-*.json` files without a managed health file, pins that client, and probes it with the Yazi actor `ping`. The filename supplies the client ID. Polls attempt a probe every 2 seconds when the serialized request queue is idle. A successful actor reply establishes liveness; an acknowledgement older than 3 seconds is offline. Normal reply polling still has its approximately 10-second timeout. A client picker remains pending. The block keeps its chosen client across reload and does not follow whichever client later writes the newest snapshot.

Ordinary Yazi snapshots have no age expiry and may remain after their producer exits. They are candidate state, not live evidence: they do not automatically open panels, and an old snapshot alone cannot enable file actions or render the live browser. Managed heartbeat or ordinary actor acknowledgements gate actions without a redundant header badge. An absent/offline producer renders a connection/offline card with Check Again and close instructions. Ordinary polling performs the live probe; Ctrl+R invokes native selection inversion.

Outside input editing or Trash confirmation, `q` closes the block. Esc cancels local path/shell input or confirmation; find/filter editing also clears its corresponding native state. In normal online operation Esc dispatches native `escape` to Yazi, preserving upstream ordering rather than unconditionally clearing the filter: visual mode exits before a retained filter, and an active find clears before a retained filter. Esc only closes the block when offline. Managed closure requests backend shutdown and returns control to the launching shell. Ordinary attached closure exits the block while leaving its independently launched Yazi running.

### Breadcrumbs and directory editing

The path header renders independent directory buttons with full-prefix tooltips and hover feedback. Home abbreviation applies only to the actual home directory or its descendants. Each click sends a pinned `cd` request; Yazi checks the target with `fs.file`, rejects non-directories without changing liveness, executes `cd`, and publishes the resulting snapshot before acknowledging.

The distinct trailing hit target replaces the breadcrumbs with a native single-line input. Enter submits; Escape cancels without navigation. Entering path mode selects the current path once; pointer focus releases that initial select-all, so repeated clicks place/edit the caret instead of selecting the whole path again. Text edits use UTF-16 boundaries, preserving Unicode paths. Absolute paths, relative paths and `~/` are accepted; spaces are preserved and `..` is left to the filesystem so symbolic-link semantics remain intact.

The width budget retains the current directory and folds older ancestors behind an ellipsis button. The ancestor layer has explicit hit testing and scroll containment; its directory buttons use the same exact-prefix targets and tooltips. Individual names truncate visually; labels and navigation targets retain their complete paths. The real interaction suite includes ancestor clicks, repeated-click caret editing, input/cancel, Unicode paths, narrow layouts, full-prefix tooltip assertions, directory-only rejection and recovery. Visual hover feedback requires inspection of the rendered breadcrumb screenshot as well as the node titles.


## 4. Inbox and wire protocol

### Runtime location and trust boundary

All three native participants resolve the inbox as:

```text
$XDG_RUNTIME_DIR/tern-yazi
```

If `XDG_RUNTIME_DIR` is unset or empty, the inbox is `/tmp/tern-yazi`. Use the same value in the Tern host/window and Yazi processes. The Rust supervisor traverses the runtime path using directory descriptors and no-follow opens; an explicit runtime directory must belong to the user and have no group/other write permission. It validates the inbox owner, sets the inbox to mode `0700`, and creates exclusive regular control/lock files with mode `0600`. Control reads enforce private permissions; Yazi snapshots retain the caller's umask inside that private directory. All reads enforce ownership, regular-file type, and byte caps. Teardown uses the pinned directory descriptor and removes only its own known/session-prefixed files. Tokens bind local session controls and reload handshakes, not an external network authentication scheme.

Ordinary Yazi setup still creates its inbox through an unquoted `mkdir -p` shell command and does not enforce private permissions on an existing directory. Choose a trusted, user-private runtime root with a shell-safe path for either mode. The inbox contains local paths and selection metadata in plaintext. Do not use a shared/untrusted inbox or describe the protocol as a sandbox.

Managed session files are `managed-CID.json` (atomic health record), `managed-CID.lease` (block acknowledgement/dimensions), `managed-CID.stop` (stop token), and `managed-CID.lock` (ID reservation). The health JSON contains `client_id`, `token`, `owner_pane`, `cwd`, and a monotonically increasing `seq`. Leases contain the token, block pane, rows, columns, and reload nonce. A matching stop token authorizes shutdown; fresh lease content/mtime maintains attachment. These records describe supervisor life independently of `state-CID.json`.

### Snapshot schema

A representative snapshot is:

```json
{
  "ts": 1791580000.25,
  "seq": 42,
  "client_id": "987654323",
  "cwd": "/tmp/example",
  "files": ["a.txt", "child"],
  "file_dirs": {"child": true},
  "parent": {"cwd": "/tmp", "files": ["example", "other"], "file_dirs": {"example": true}},
  "preview": {"cwd": "/tmp/example/child", "files": ["sub.txt"], "file_dirs": {}},
  "filter": false,
  "selected": 1,
  "selected_urls": ["/tmp/example/a.txt"],
  "mode": "normal",
  "marked_urls": [],
  "hovered": {
    "url": "/tmp/example/child",
    "dir": true,
    "size": 4096,
    "mtime": 1791580000,
    "selected": 1
  },
  "tasks": {"total": 2, "succ": 1, "fail": 0, "found": 1024, "processed": 512}
}
```

`files` preserves Yazi's filtered/sorted current listing; `file_dirs` maps visible entry names to directory booleans without filesystem scanning. `parent` and `preview` provide bounded directory snapshots (at most 30 entries each). `filter` is an authoritative string when an active filter is applied, `false` when inactive, or `null` if unexposed; it is included in snapshot deduplication. `frontend/input.sync_hover()` syncs `filter_query` unless local filter editing or `backend/actions.pending_filter(state)` indicates a pending filter mutation. Unrelated escape/action acknowledgements do not block authoritative filter synchronization. Persistent `selected_urls` are sorted absolute paths. `mode` is `normal`, `select`, or `unset`. `marked_urls` carries visual-range marks, separate from persistent selection. Hover and parent objects can be absent; protected task reads fall back to zero counters on failure. `client_id` comes from `YAZI_ID`, with `default` as fallback. Managed startup supplies its generated numeric ID; ordinary attach fixtures should use the original executable, for example `/usr/bin/yazi --client-id 987654323`, because the managed wrapper reserves that option. Host attach discovery accepts numeric filenames.
### Request serialization

`backend/actions.luau` maintains the host VM's FIFO queue and sends one request at a time. Each request freezes its pinned destination client at enqueue time. IDs combine an epoch from `tern.now()`, the pane identifier supplied by the frontend coordinator, and an incrementing counter; managed requests additionally prefix the client ID and session token, allowing the supervisor to clean up its own replies. Enqueue/dispatch check client identity and liveness. The Yazi executor validates IDs against `^[%w-]+$` before constructing reply paths.

The process invocation is an argv array:

```text
ya emit-to <client-id> plugin tern <quoted-request-json>
```

The JSON must additionally be quoted for Yazi's plugin-argument parser. `pump()` wraps the encoded JSON in single quotes and escapes embedded apostrophes independently of process argv boundaries. Keep this second parsing layer when changing serialization.

Mixed Lua tables cannot safely encode both positional and named arguments as a JSON array. The backend splits them into explicit wire members; Yazi reconstructs numeric and option keys before calling its actor:

```json
{
  "id": "123456-7-1",
  "op": "action",
  "action": "reveal",
  "args": {
    "positional": ["/tmp/example/c.txt"],
    "options": {"no_dummy": true}
  }
}
```

Empty argument collections may encode as empty objects; the decoder treats missing/empty collections as no entries. Preserve the `positional`/`options` envelope rather than relying on JSON object keys such as `"1"` to become Lua numeric keys.

### Operations

| `op` | Main fields | Executor behavior |
| --- | --- | --- |
| `ping` | Request ID | Forces a snapshot and returns an actor acknowledgement for ordinary attach liveness |
| `command` | `action`, `args`, optional `target`/`cwd` | Dispatches native `shell` commands (blocking visible PTY terminal or non-blocking background task) |
| `action` | `action`, `args` | Dispatches native companion actors (`cd`, `arrow`, `leave`, `enter`, `back`, `forward`, `reveal`, `yank`, `toggle`, `toggle_all`, `visual_mode`, `escape`, `hidden`, `find_arrow`) via `ya.exec` and forces snapshot |
| `activate` | `target`, `action` | Reveals exact directory `target` via `reveal` then executes native `enter` (`op = "activate", action = "enter"`) |
| `toggle_advance` | None | Executes native `toggle` then `arrow 1` sequentially |
| `filter` | `query`, `done` | Calls `filter_do` with `smart=true` and `done=req.done~=false` (in-flight typing or Enter commit) and forces snapshot |
| `find` | `query`, optional boolean `previous` | Calls `find_do` with `smart=true` and `previous=req.previous==true`, moves the native cursor without filtering the listing, and forces snapshot |
| `trash_prepare` | `target` | Returns authoritative target paths without mutating Yazi or the filesystem |
| `trash_commit` | `paths` | Validates each file and runs the guarded confirmed-trash pipeline |

### Replies and completion meaning

Yazi writes a reply to `reply-<id>.json` by writing/closing a `.tmp` file and renaming it. Replies contain `id` and `ok`, plus `error` on failure, or operation-specific `paths`, `queued`, and `confirm_trash` fields.

A successful `ya` exit only proves transport dispatch. After it exits successfully, the backend polls the reply file immediately and then every 50 ms, up to 200 attempts (approximately 10 seconds). A matching reply ID is consumed and its file removed. The next queued request begins only after the current one finishes. A failure clears the remaining queue and optimistic cursor/cwd, and reports the actual error to the frontend for presentation. Spawn errors, nonzero transport exits, invalid/missing replies, and timeout are not reported as success.

An `ok` reply acknowledges actor acceptance or the target pipeline's settled dispatch, not completion of an opener, shell process, trash job, or filesystem task. Inspect Yazi task results and fixture files for end-to-end outcomes. Timeout explicitly means the outcome is unknown; do not blindly retry a destructive request. Late/unconsumed replies can remain in the inbox.

### Native shell interception and terminal lifecycle

Managed shell commands execute through a private `PATH` shim directory (`shim-<cid>-<token>`) created by the supervisor and placed at the head of Yazi's `PATH`. When Yazi dispatches a blocking shell execution, the shim script executes `tern-yazi-launch --native-shell [args]`:

1. **Request creation**: `--native-shell` reads `managed-<cid>.json` and the active lease to verify the supervisor binding. It creates `native-shell-<id>.json` in the runtime inbox, where `id = <cid>-<token>-<pid>`, containing `{ version = 1, id = id, cid = cid, token = token, pane = pane, owner_pane = owner_pane, cwd = cwd, argv = argv }`.
2. **Routing and verification**: The registered `tern.route.open` handler delegates to `frontend/window.luau`; `backend/window.luau` claims and verifies the request's `id`, `pane`, `cid`, `token`, `owner_pane`, and active lease. The host coordinator also calls `backend/actions.poll_native_shells` to monitor pending requests and report a plain open-path effect for timely routing if the window route is delayed.
3. **Terminal launch**: Upon verification, the backend returns terminal launch data. The window frontend applies it by splitting the companion pane to the right (`cx.layout:split`) running `tern-yazi-launch --native-terminal <id> <path>` in a real PTY terminal, and zooms the terminal pane.
4. **Execution and synchronization**:
   - When the terminal opens, it creates `native-shell-<id>.started` containing the request ID.
   - The terminal runs the command with full stdout, stderr, and interactive stdin.
   - Upon command exit, it atomically writes `native-shell-<id>.result` containing `{"id": id, "status": status}`.
   - The waiting `--native-shell` shim reads `native-shell-<id>.started` and `native-shell-<id>.result`, reaps the exit status, cleans up protocol files, and exits with the command's actual exit status back to Yazi.
5. **Terminal completion**: The terminal displays the printed exit status (`[exit %s] Press Enter to return to Yazi...`). When the user presses Enter, the terminal pane exits and closes. On `pane_closed`, the window restores the prior zoom state to the companion or owner pane.

**Fallback**: In ordinary attach mode without a managed supervisor, or if managed request creation encounters an unattached session, `--native-shell` falls back to direct `/bin/sh` execution without terminal handoff.

Non-blocking shell commands submitted via raw `;` input (`shell { run, block = false, orphan = false }`) directly queue native Yazi background tasks without invoking the blocking terminal wrapper, returning `{ ok = true, queued = true, orphan = false }` and displaying an informational toast while keeping the browser active.

The shell execution environment for launched commands preserves the user's original `PATH`, removes internal bridge and Tern pane identifier variables, and restores the actual controlling terminal description (`TERM` and PTY modes). The execution environment maintains the caller's umask and process group boundaries without exposing or publishing plaintext values of sensitive environment variables.
### DDS telemetry is a separate interface

The Yazi plugin still publishes `tern-hover`, `tern-cd`, and deduplicated `tern-state` telemetry via `ps.pub_to(0, ...)`. It registers `ps.sub_remote("tern-cmd", ...)`: `{"op":"hello"}` republishes folder/hover telemetry, while other bodies produce a `tern-ack` echo, followed by a pulse. This capability is necessary for inbound custom DDS kinds in Yazi 26.9.

The native companion does not run `ya sub` or use this DDS stream as its view transport. Its state comes from snapshot files, and its command requests enter `M:entry()` through `ya emit-to ... plugin tern`. Do not confuse DDS echo acknowledgements with per-request actor replies.

## 5. UI state and event wiring

### UI architecture and boundaries

The native Luau implementation separates presentation from runtime coordination. Root `host.luau` and `window.luau` retain the manifest entry paths but only register their respective frontend handlers. Frontend modules render native UI, handle input, and apply block/window effects. Backend modules own snapshots, actor IPC, liveness, process/file I/O, preview data, and lifecycle bookkeeping. The Rust supervisor and Yazi Lua actor remain the external authorities described above; this is a module-boundary refactor, not another process, daemon, Python runtime, or Yazi core patch. The existing `companion.css`, UI behavior, protocol records, and timing remain the contract.

The dependency direction is entry -> frontend -> backend -> shared helpers. Backend modules may use pure model/path helpers and other backend modules, but cannot import frontend modules. Shared helpers perform no I/O and do not depend on either runtime half. Keep the graph acyclic and reuse snapshot/string references rather than cloning data simply to cross a layer.

- Frontend modules must not call `tern.fs` or `tern.process`, and must not read/write inbox protocol records themselves. They own `tern.ui` nodes, native input, `BlockCx`/`EffectCx`/window presentation contexts, and presentation effects such as toast, open, block creation/exit, focus, zoom, and layout.
- Backend modules must not import `tern.ui`, construct `Node`/`ui.*`, or call presentation methods. Non-mutating pane metadata is supplied by the frontend or an explicit query callback. Completion is reported as plain results/events or `on_change` callbacks; the owning frontend coordinator decides which native effect or render to perform.
- File-preview reads return data, not native nodes. Uploading image bytes through `cx:blob` remains a frontend presentation adapter; it is not backend file I/O. Short preview warnings are preserved without forwarding raw exceptions into the UI.

Tern's verified [Packages and Manifests: Modules and `require`](https://docs.stencil.so/tern/concepts/packages.html#modules-and-require) contract supports package-local `require('./module')` and `require('../module')`, resolved relative to the calling file. For example, a frontend module imports `require('../backend/session')`, while backend modules can import `require('./inbox')` and `require('../shared/path')`. Paths cannot leave the package. Modules are cached by resolved file within each VM: the host and each window VM load independent instances, so a shared module is not a cross-VM singleton. Cross-VM state continues to flow through the existing inbox records and plugin-private `tern.kv` store, not module globals.

### Shared module contracts

`shared/path.luau` exports the existing pure helpers. `basename(path?)` returns the final path component, with `Yazi` for an absent/empty path. `safe_cid(value)` normalizes an ID to a string (numeric IDs use integer formatting); it is not numeric-ID validation. `join_path(dir?, name?)` combines the existing components without filesystem resolution or path normalization.

`backend/inbox.luau` exports `state_dir()` for the shared runtime location, `read_json(path)` for capped 256 KiB reads returning a decoded table or `nil` on read/decode failure, and `lease_hint(cid)` returning `(token, pane, nonce)`. Lease-hint reads are capped at 4096 bytes; parsing retains the existing two-field fallback with no nonce. A parsed lease is still only a hint: matching identity and the current nonce acknowledgement are required for managed adoption.

`backend/preview_policy.luau` exports the pure `limits` map (category `default` and `max`) and `defaults()` for the settings value. Both host preview reads and the window configuration command use this single unchanged policy; each VM still has its own module instance.

### Callback and coordinator ownership

The layers execute in the existing host/window VMs, not separate frontend/backend processes. Tern's [native callback lifecycle](https://docs.stencil.so/tern/concepts/runtime.html#timers-process-and-fetch-callbacks) serializes timer/process callbacks with other handlers in each VM. Processes run off the VM thread, but their completion callbacks return to that VM; pending callbacks end with the VM at reload.

Frontend coordinators own presentation contexts and consume backend change/completion notifications. A backend completion can change model/request data or report a plain result, but does not render, show a toast, open a file, launch a native block, or alter pane layout. The block coordinator retains the latest context supplied by `view` for timer-driven effects and lease dimensions; the window coordinator owns pane/layout effects around backend identity and reconciliation decisions. This preserves the existing callback ordering, 500 ms polling, reload adoption, and exactly-once file/shell routing rather than introducing another scheduler.

The host coordinator's `register()` owns block/hook registration. It assembles one block state with `session.new(args, saved, pane)`, `input.init(state)`, and `preview.init(state)`, then wires callbacks before `session.install(state)` starts polling:

- `on_event(event)` consumes the backend events `toast`, `open`, `restore_command`, and `reset_input`; native toast/open effects stay in `frontend/host`, while input restoration/reset goes to `frontend/input`. A liveness/request failure reports `reset_input` with an optional `clear_trash` flag instead of having the backend mutate controller state.
- `on_change()` renders through the current block context. `on_poll(yz)` coordinates shell discovery and managed lease refresh or an ordinary actor probe. `on_close()` clears owned action handoffs and releases the retained presentation context.
- `session.read_latest_state`, `refresh_live`, `managed_poll`, `close`, and `save` own protocol/session operations. The frontend injects the read-only `state.pane_list` query; `managed_poll` receives plain `{pane, rows, cols}` metadata, never a native context. `start_discovery(can_open, on_open)` reports a verified managed-record path to the frontend's retained shell effect context; it does not open the path itself.
- `actions.request(state, yz, body, done?)` and `action(state, yz, name, args)` own the serialized actor queue; request completion receives a plain reply. `open_selected`, `open_shell`, their handoff-event handlers, and `terminal_here` own I/O and report presentation/completion events rather than using a block context.
- `actions.pending_filter(state)` returns whether the owned request queue contains a pending filter mutation, without allocating another queue/list. Input synchronization uses this narrower query rather than waiting for unrelated requests to settle.
- `frontend/input` exports `init`, `close`, `reset`, `restore_command`, `sync_hover`, `key`, and `event`. It owns editing, optimistic cursor/cwd, and trash-confirmation controller state and calls action APIs. `frontend/view.render(state, cx, yz, prepared, preview_data)` owns every native node, consuming pure `model.presentation(state, yz)` data and the frontend-prepared preview payload.
- `frontend/format` exports `fmt_size` and `file_icon` for display labels and glyph/tone selection. These presentation policies are separate from the backend model's semantic file classification.

`frontend/view` depends on `frontend/format` and the pure model/path helpers; it does not call the preview backend. `frontend/format` uses only the backend model's semantic extension/archive classification. `frontend/host` coordinates `input`/`view` with `session`/`actions`/`model`/`preview`. On the backend side, `actions` depends on `session`/`inbox`, `session` on `model`/`inbox`, and `preview` on `model`/`session`/`preview_policy`; path helpers are shared without reverse dependencies.

The block keeps a single state object across those modules; `session.current(state)` is the common ownership guard for asynchronous results. This is not a cloned frontend/backend state pair.

### Window coordinator contract

`frontend/window.register()` registers the managed/shell/files/native open routes, window-start and pane-close handlers, the 500 ms timer, toggle/preview-limit commands, and `Ctrl+Alt+Y`. It depends on `backend/window`, which in turn depends only on `inbox` and the pure `preview_policy`. The frontend supplies read-only `query.pane(id)`, `query.tab_of(id)`, and `query.zoomed(id)` callbacks; the backend never receives a `cx` or a presentation callback.

- Managed discovery uses `discover_managed(panes)` and `next_managed(scan)` to consume candidates in order. For each candidate, `managed_launch_plan`/`managed_launched`, `managed_active`, `managed_lease`, and `managed_poll` surround the frontend's actual launch/adoption and event delivery. Preserve discovery -> frontend launch/adopt -> lease verification ordering; do not collect all candidates and delay their presentation until after the scan. `finish_managed_scan` returns offline pane IDs. `focused_managed`, `managed_closed`, and `managed_request` supply toggle, restoration, and wake decisions.
- `shell_request`, `files_request`, and `native_request` return either no match or plain handled-route data: an unavailable/message result, an event to send, a terminal launch plan, or a frozen file batch. The frontend alone calls split/open/close/zoom/toast and sends native session events. `shell_completed`, `files_completed`, and `native_completed` record/report the outcome of those effects; a native request is persistently claimed before its launch plan is returned.
- `expire_native_terminals` returns terminal pane IDs to close; `shell_closed` returns the previous restoration record; `expire_handoffs` returns events to deliver. Protocol bookkeeping and cleanup remain backend-owned, while layout restoration remains frontend-owned.
- `preview_config_changed()` reports whether the frontend should send ordinary block poll events. `initialize_preview_limits()` uses `preview_policy.defaults()` before the frontend opens the settings file.

The backend retains managed/closed clients, shell/file pending handoffs, reload nonces, and the `shell_terminals`/`native_shell_requests` KV registries. The frontend owns `managed_overlays` KV entries and local restoration flags because they represent UI intent, not protocol authority. These module tables remain VM-local; the existing shared inbox/KV contracts provide reload and cross-VM continuity.

### Authoritative versus optimistic state

Yazi snapshots own the current directory listing, cursor, persistent selection, visual marks, and tasks. The companion retains only local editing state, a provisional `active_file`/`optimistic_cwd`, pending trash paths, request queue, and archive cache. Presentation/input state stays with the frontend; protocol coordination and preview data belong to backend modules, without creating another authoritative file-manager state.

`sync_hover()` clears optimistic state when the source client, cwd, or hovered URL changes. A task-only or selection-only snapshot with those three values unchanged must not discard a click that is still being dispatched. This distinction is important for the click-then-keyboard regression: a click requests an actual Yazi `reveal`, and later `j`/`k` moves from that real cursor, not from a Tern-only highlight.

Current, parent, and preview child columns render directly from authoritative Yazi snapshots (`yz.files`, `yz.parent.files`, `yz.preview.files`) and their companion `file_dirs` mapping. The companion performs no recursive filesystem enumeration (`tern.fs.list`) for type probes or directory previews. Bounded snapshot limits remain unchanged: state JSON payload is capped at 256 KiB, and parent and preview directory entries are bounded to a maximum of 30 items. Filtering is performed natively by Yazi; the companion does not maintain a second local regex filter.

### List identifiers and events

Stable list paths are:

```text
main.browser.columns.parent.parent_list
main.browser.columns.current.files_list
main.browser.columns.preview.child_list
```

Rows use filenames as stable keys and full paths as `title` tooltips. Native list selection is a complete item node ID, not a row number. `list_event_name()` removes the exact `<list-id>.` prefix from `ev.item`; the handler checks that the resulting name belongs to the current list before acting.

| Event | Behavior |
| --- | --- |
| Current-column `select` (single click) | Set provisional filename and send `reveal(target, no_dummy=true)` to Yazi |
| Parent/child `select` | Provisional cwd becomes the selected entry's containing directory; reveal the exact entry in Yazi |
| Any list `activate` on a directory (double click) | Reveal exact target then execute native `enter` (`op = "activate", action = "enter"`) |
| Any list `activate` on a file (double click) | Open target file directly through Tern (`cx:open`) |

The handler accepts both `select` and `activate` using the item's semantic ID. Directory navigation (`enter`, `leave`, `cd`) is handled natively by Yazi. File opening and previews are owned entirely by Tern: pressing `o` or `Enter` on files expands all authoritative selected paths (`selected_urls`) directly through Tern (`cx:open`), or opens the hovered file if none are selected; double-clicking a file opens the exact clicked file directly through Tern (`cx:open`) without invoking a native actor; pressing `Enter` on an unselected directory remains native `enter` in Yazi. File previews render natively in Tern.
Green check marks represent persistent selected paths. The native selected-row capsule represents cursor position. Amber `+`/`-` marks represent the visual range (`select`/`unset`); these are not persistent selection marks. Counts remain based on Yazi's snapshot while the cursor moves.

### Regions, layout, and actions

The block returns `main`, `dock`, and `layer` regions. Region roots are stripped by Tern, so the footer row is wrapped as a child of a `ui.col` in `dock`. The confirmation lives in `layer` with its own opaque card and pointer-enabled controls.

The top path display uses a dedicated path card (`yazi.path-header`) with single-line truncation and a full-path tooltip. The following row is reserved for input: blank in normal mode, occupied by shell, find or filter editing. Selection counts remain in the docked footer. Column card headings are single-line and truncate with ellipsis.

CSS assigns parent/current/preview shares of `4fr / 9fr / 7fr`, each with `minmax(0, ...)`. Pane height is inherited through flex/grid containers with `min-height: 0`; only native list scrollers and the preview region scroll. Lists retain `max.lines` and full selected node IDs for native selected-row reveal. Do not pad lists to terminal rows or remove the native selection-reveal property. Image wrappers constrain natural raster/SVG dimensions and use `object-fit: contain`.

Clickable controls use action strings such as `open_file=<path>` (opens file directly through Tern via `cx:open`), `enter_dir=<path>` (requests native `activate` enter), `copy_path=<path>`, and `terminal_here=<cwd>`, delivered to the handler as `ev.act`/`ev.value`. `event()` also accepts a structured `shortcut` action carrying `ev.key`; both that route and the block's `key()` call the same `handle_key()` implementation.

Preview column action buttons offer quick actions: "Open in Tern" opens the target file directly through Tern (`cx:open`), "Copy Path" copies the target path to the system clipboard, and "Terminal Here" launches a new terminal pane split via Tern with explicit source pane tracking and launch error reporting. Custom archive extraction mutations and extraction buttons are removed; archive preview listings remain strictly read-only. Status display presents actual metadata without hardcoded permission strings. Keyboard shortcuts dispatch native Yazi actions: `y` dispatches native yank (copy) and `x` dispatches native yank with `cut = true`, where Yazi clears the selection automatically.
## 6. Keyboard and local command semantics

### Normal mode

| Key | Behavior |
| --- | --- |
| `j` / Down, `k` / Up | Yazi `arrow` by `1` / `-1` |
| `h` / Left | Native `leave` (parent directory; Backspace does not leave) |
| `l` / Right | Native `enter` (enter hovered directory) |
| `H` | Native `back` (history back) |
| `L` | Native `forward` (history forward) |
| `o` | Open authoritative selected file(s) or visible hovered file directly through Tern (`cx:open`) |
| Enter | Enter directory via native `enter`, or open authoritative selected/hovered file(s) directly through Tern (`cx:open`) |
| Space | Native `toggle_advance` (composite sequence: executes `toggle` then `arrow 1`) |
| `v` | Native `visual_mode` |
| `V` | Native `visual_mode` with `unset = true` |
| `y` | Native `yank` (copy; Yazi clears selection itself) |
| `x` | Native `yank` with `cut = true` (cut; Yazi clears selection itself; not archive extraction) |
| `d` | Prepare target paths via `trash_prepare` and show native trash confirmation |
| `gg` / Home | Native `arrow top` (`g` acts as leader key; second `g` jumps to top; state resets on other key/Esc or cwd change) |
| `G` / End | Native `arrow bot` |
| PageDown / PageUp | Native `arrow 100%` / `-100%` (Yazi viewport units) |
| Shift+PageDown / Shift+PageUp | Half-page semantic handler (`arrow -50%` / `50%`), subject to Tern routing below |
| Ctrl+D / Ctrl+U | Half-page semantic handler (`arrow 50%` / `-50%`), subject to Tern routing below |
| Ctrl+F / Ctrl+B | Full-page semantic handler (`arrow 100%` / `-100%`), subject to Tern routing below |
| `f` | Edit live native smart filter (`filter` requests dispatched with `smart=true` as text changes) |
| `/` / `?` | Edit native smart find forward/backward (`find_do` with `smart=true`, boolean `previous`); moves the cursor without hiding entries |
| `n` / `N` | Native `find_arrow` next/previous match (`previous=false` / `true`) |
| `;` | Edit a raw non-blocking shell command locally (`shell --interactive`); submits native Yazi background task returning browser promptly (not orphan, no terminal) |
| `:` / Shift+`;` | Edit a raw blocking shell command locally (`shell --block --interactive`); launches a visible PTY terminal with stdout/stderr/stdin until Enter returns to Yazi |
| `.` | Yazi `hidden` with `state="toggle"` |
| Ctrl+A | Native `toggle_all` with `state="on"` |
| Ctrl+R | Native `toggle_all` (toggle all, replacing former refresh) |
| Esc | Sends native Yazi `escape` when online (visual mode and find clear before a retained filter; selection follows upstream handling); cancels local input/trash and clears the corresponding native find/filter while editing; closes companion only when offline |
| `q` | Close outside local editing/confirmation; restore owner shell in its prior zoom state, or leave ordinary attached Yazi running |
Shifted letters have explicit actions for `G`, `H`, `L`, `N` and `V`; other shifted letters are not silently treated as their unshifted actions. Normal-mode Alt/Meta chords are not claimed by the handler.

### Input editing

Find (`/`/`?`), filter (`f`) and shell (`;`/`:`) input bars accept printable key text, Space, paste, and Unicode. Backspace removes one UTF-8 codepoint, not an entire grapheme cluster. Enter submits the current text; a successful submission leaves editing, while the command bar remains available after a submission error.

Find editing sends native `find` requests as text changes and again on Enter, retaining Yazi's find state for `n/N` after the prompt closes. `/` searches forward; `?` searches backward via the boolean `previous` flag. Smart matching delegates case handling to Yazi. Find moves hover through the existing listing, including an already filtered listing; it is not a filter alias. Esc while editing closes the prompt and sends `escape` with `find=true`, leaving an existing filter intact.

Filter editing (`f`) sends `filter` requests with `smart=true` and `done=false` as text changes. Enter sends `done=true` and leaves editing while retaining the filter. Lowercase smart-filter queries match either case; uppercase queries are case-sensitive. Esc while editing clears the filter query and native filter. Active filter state is reported via `yz.filter` in snapshots (`string` when active, `false` when cleared). In normal mode, native Esc first leaves an active visual mode without removing the retained filter; another Esc clears that filter. With an active find over a retained filter, the first Esc clears find and leaves the filtered rows intact; the following Esc clears the filter. Shell/path input and Trash confirmation cancel locally without navigation or a Trash mutation.

### Unsupported actions and keys

The companion aligns strictly with implemented upstream Yazi actions. Specific unsupported inputs include:
- Key combinations such as `cc`, `p` (paste), and `r` are not implemented and are not emulated by synthetic frontend actions.
- Backspace in normal mode does not execute `leave` (it only erases text during input bar editing).
### Shell execution semantics

Shell keybindings adhere directly to upstream Yazi semantics as defined in primary documentation ([Quick Start](https://yazi-rs.github.io/docs/quick-start)) and shipped configuration ([`preset/keymap-default.toml`](https://github.com/sxyazi/yazi/blob/shipped/yazi-config/preset/keymap-default.toml)):

- **Non-blocking background shell (`;`)**:
  Mapped to `run = "shell --interactive"`. The `--interactive` flag opens the input prompt bar in the footer. Submitting a command dispatches `shell { run, block = false, orphan = false }` as a native Yazi background task. No terminal pane is opened and the browser returns promptly; task progress and completion are reported natively by Yazi tasks. This is a tracked background task, not an unmonitored orphan process.

- **Blocking visible shell (`:`)**:
  Mapped to `run = "shell --block --interactive"`. The `--interactive` flag opens the input prompt bar in the footer. Submitting a command dispatches `shell { run, block = true }`, initiating a host-tracked terminal handoff. This opens a real visible PTY terminal pane in a zoom overlay, executing the command in Yazi's working directory with stdout, stderr, and interactive stdin. Upon process exit, the terminal displays the exit code (`[exit %s] Press Enter to return to Yazi...`). Pressing Enter closes the terminal pane and returns directly to the live Yazi browser session in the zoom overlay.

Neither mode requires or accepts a `shell` command prefix—users type the raw shell script directly. The prior custom manager-command parser has been removed in favor of direct upstream shell routing. Raw user text is passed without frontend flag parsing (e.g. typing `--orphan` is treated as literal shell script arguments, not an actor option). All variable and macro expansion (such as `%y`, `%t`, `$0`, `$@`) is delegated directly to Yazi rather than simulated by a frontend template or list parser. Shell syntax, quoting, expansions, and side effects belong to the shell execution environment, running in the active working directory without wrapper modifications.
### Tern 0.7.0 input-routing boundary

The host implements full/half-page semantics when those events are delivered. In the observed Tern `0.7.0` runtime, default scoped preset actions intercept Ctrl+D/U/F/B before plugin key callbacks, and Shift+PageDown/Up do not reach this block. Plugin bindings for preset-owned chords are rejected, and attempts to override the scoped action IDs return `unknown command`. These public plugin APIs therefore do not provide a working route for the intercepted physical keys.

Plain PageDown/Up remain the exercised paging path. Do not label the intercepted chords functional based only on host handler code, and do not silently rewrite persistent user keybindings to bypass the routing boundary. Any future routing change needs a real key-event regression against the target Tern build.

## 7. Preview configuration and rendering

Run `Yazi: Configure Preview Limits` from the command palette (`plugin.tern-yazi.preview_limits`). The command initializes missing defaults in the plugin-private store and opens:

```text
tern.plugin.data/kv.json
```

Edit the `preview_limits` member and save the file. Preserve other store keys:

```json
{
  "preview_limits": {
    "image_bytes": 16777216,
    "markdown_bytes": 1048576,
    "code_bytes": 1048576,
    "mermaid_bytes": 262144
  }
}
```

| Field | Default | Maximum | Preview categories |
| --- | --- | --- | --- |
| `image_bytes` | 16 MiB | 16 MiB | PNG, JPEG, GIF, WebP, SVG |
| `markdown_bytes` | 1 MiB | 4 MiB | `.md`, `.markdown` |
| `code_bytes` | 1 MiB | 4 MiB | Other file content passed to text/code preview |
| `mermaid_bytes` | 256 KiB | 1 MiB | `.mermaid`, `.mmd` |

Values are positive, finite integer byte counts. Missing/invalid fields use their category defaults; values above the category maximum are clamped. A missing store is empty. Invalid JSON or an unreadable store displays the concise warning `Preview settings unavailable` instead of silently applying a different configuration.

`tern.kv.get` rereads the file when it changes on disk, shared across the plugin's host/window VMs. The window's 500 ms configuration-stamp check sends a normal `poll` event to existing blocks when the encoded store value or error changes. Each block's own 500 ms timer also checks its configuration stamp, snapshot timestamp/sequence, and live state, rendering when those values change independently of window frames. The preview backend resolves limits on each preview build, so saving a corrected configuration retries the read without needing a Yazi cursor move or source reload. These are plugin-private settings, not global Tern preferences.

`tern.fs.read(path, limit)` rejects a whole oversized file before allocating/reading its content. Its API requires a regular non-symlink target, bounds raced growth, and avoids blocking opens of nonregular targets. It does not return a truncated prefix. A bounded-read failure displays `Preview unavailable (<active limit> limit)` in `yazi.preview-unavailable`; settings and image-blob failures display concise unavailable warnings. Preview status never renders the raw runtime exception, traceback, `max_bytes` diagnostic, or internal source location. Preview byte caps do not modify image geometry or guarantee bounded rendered DOM complexity.

The image cap matches Tern's documented [16 MiB single-blob limit](https://docs.stencil.so/tern/protocol/operations.html#security-and-limits). Successful bounded reads and blob creation do not guarantee that an image decoder accepts the content. In the observed runtime, a 10,837,739-byte PNG decoded into native image/zoom UI, and a 183-byte SVG decoded; a roughly 9 MiB SVG dominated by an XML comment remained a missing-image placeholder. Treat that SVG case as a renderer boundary rather than a plugin read-limit failure. Image byte settings cannot override native decoder/parser limits.

### Data-only preview contract

`backend/preview.init(state)` initializes the existing archive cache; `sync(state, yz)` maintains its snapshot stamp, and `config_stamp()` exposes the settings change marker. `prepare(state, path)` returns one plain tagged payload, never a UI node or blob handle:

| `tag` | Data contract |
| --- | --- |
| `settings-unavailable` | Concise settings failure; no raw exception |
| `unavailable` | Active byte `limit` for the bounded-read warning |
| `archive` | `status` (`loading`, `ready`, or `empty`) and the existing cached `items` reference |
| `image` | Bounded `content` bytes, `mime`, and `ext` |
| `mermaid`, `markdown`, `binary` | Bounded `content` string |
| `code` | Bounded `content` string and detected `lang` |

For a non-empty `image` payload, `frontend/host` performs `cx:blob(content, mime)` under protection and adds the successful blob ID to that transient payload. `frontend/view` builds the image node, or the existing short unavailable/empty-file presentation. Text and archive payloads similarly become native nodes only in the view layer. Snapshot listings, cached archive items, and bounded strings retain their references across this boundary; the adapter does not copy the authoritative snapshot or move file reads into the frontend.

### Native rendering dispatch

Rendering dispatch is extension-based:

- Images/SVG use bounded raw bytes, `cx:blob(content, mime)`, and a native `image` node. Empty images show an empty-file message; blob failures show `Image preview unavailable` without raw exception details.
- Mermaid is wrapped in a Mermaid Markdown fence and passed to `ui.md`.
- Markdown uses `ui.md(content)`.
- Other files with NUL bytes show a binary-file message; otherwise `ui.code(content, language)` applies the extension language map, falling back to `text`.
- A hovered directory uses the authoritative child preview snapshot from Yazi (`yz.preview`, capped at 30 entries) and `file_dirs`, without local filesystem enumeration.
- Archives use asynchronous external listing rather than `read_preview`: ZIP uses `unzip -Z1`; tar-like formats use `tar -tf`; `.7z` uses `7z l -ba -slt`; other recognized archive extensions fall back to tar. At most 50 output lines are cached and at most 18 displayed. This is a bounded line display, not a full archive-entry parser. The URL-keyed cache belongs to the current `client_id:seq:cwd` preview stamp and is cleared when that stamp changes; it has no archive-modification watcher. A process callback accepts its result only for the same non-closing current session and cache, then calls `on_change()` for frontend rendering.

There is no dedicated PDF, audio, or video viewer. Archive previews provide read-only file listings; custom archive extraction mutations and extraction buttons have been removed in favor of Yazi's native archive openers and rules. Recognizing an archive extension for listing does not imply in-place mutation support.
## 8. Exact-target safety and regressions

### Target calculation

`targets()` starts with Yazi's persistent selection, adds visual marks with `is_marked()==1`, removes marks with `is_marked()==2`, and sorts the resulting absolute paths. Only if that result is empty does it fall back to the visible target. This authoritative calculation underlies `trash_prepare` (keyboard `y`/`x` instead dispatches native Yazi `yank` directly).

### Two-phase trash

1. `trash_prepare` returns target paths without any mutation.
2. The host freezes those paths and their originating client in `state.trash`, and displays every exact path JSON-encoded in the native confirmation layer.
3. While that layer is active, keyboard and UI actions are restricted to confirm/cancel. Cancel clears local confirmation and sends no mutation.
4. Confirm sends `trash_commit` to the frozen, pinned client. A client mismatch or offline producer prevents dispatch rather than transferring the confirmation to another client.
5. Yazi resolves every approved path through `fs.file(Url(path))` before committing.
6. A synchronous stage clears visual/selection state and toggles the approved files on. It queues the `finalize` plugin stage with `mode="sync"` behind those toggles.
7. `finalize` compares normal-mode state and the entire sorted authoritative selection to the approved paths. Any mismatch refuses the operation.
8. Only after that guard does it emit `remove` with `force=true` and no permanent-removal option. Here `force` skips Yazi's second confirmation; it does not request permanent deletion.
9. A following `settled` stage snapshots and acknowledges dispatch. Task/filesystem results still determine eventual completion.

The two-phase guarded confirmation pipeline is reserved for `trash_prepare` and `trash_commit`. Keyboard yank (`y`) and cut (`x`) delegate directly to Yazi's native `yank` actor (`ya.exec("yank", ...)`), which manages its internal yank register (not system clipboard) and clears the selection automatically without companion overrides or custom selection freezing.

### Preserve these invariants

- A single click updates the real Yazi cursor through `reveal`; the next keyboard movement starts there.
- Double-click activation reveals an exact directory target in Yazi before native `enter`; file double-click opens the exact clicked path directly through Tern, independent of old hover/selection. Keyboard `o`/Enter opens the authoritative selected file set, or the hovered file when nothing is selected.
- Task-only snapshots do not erase provisional click state.
- Current rows follow Yazi's sorting/filtering and hidden-state toggle rather than a parallel local filter.
- Native list item IDs and `args.positional` numeric reconstruction remain intact through the wire.
- Raw shell input preserves spaces, Unicode, quotes, and shell syntax across both non-blocking (`;`) and blocking (`:`) modes without custom manager-command mangling or prefixes.
- Cancelled confirmation has no Yazi/filesystem mutation. Approved paths/client remain frozen until commit.
- Transport success, actor acknowledgement, and asynchronous task completion remain distinct claims.
- Preview failures show concise user-facing warnings without exposing runtime exception details, truncating file content, or relaxing byte caps.

## 9. Reproducible isolated QA

There are two different verification layers. Plugin registration verifies manifest/VM loading. Interactive fixtures verify events, layout, actor effects, and rendered previews. Record the commands, versions, expected outcomes, and observed files/screenshots for the layer actually exercised.
### Test topology and automated validation

Integration testing requires a real GUI display environment (X11 or Wayland) with an isolated, daemon-backed Tern window. Standalone `serve` mode cannot exercise managed daemon CLI routing or window-key pane controls; the manual renderer fixture below is a separate, narrower verification layer.

Tests create a private mode-0700 artifact root containing disposable `XDG_*` directories, a private daemon/socket, runtime inbox, real files, and a `YAZI_CONFIG_HOME` sandbox linked to the repository plugin. They do not touch user configuration (`~/.config/yazi`) or global desktop settings. GUI shells may strip non-standard variables while preserving `XDG_*`, so every managed launch explicitly prefixes `YAZI_CONFIG_HOME=<ROOT/yazi>` instead of relying on ambient inheritance. The fixture shell forwards `"$@"` to real `/bin/bash --noprofile --rcfile <private-bashrc> -i`, preserving Tern's `-c` commands as well as normal interactive startup. `tern new tab --json` creates an inactive tab and returns `{session, tab, block}`; the harness deliberately focuses that exact numeric `block` ID through the daemon CLI before waiting for its prompt and launching the second client. Successful runs clean up temporary files; failures preserve their private artifacts for inspection.
The test suite consists of:
- **Rust integration harness** (`launcher/tests/interaction.rs`): Automates real Tern window control and daemon management, verifying process supervision, PTY interception, and native protocol lifecycles.
- **Shell test runner** (`tests/smoke_luau.sh`): Runs the Rust interaction suite against a real, isolated Tern window and its private daemon.

#### Prerequisites

- Rust/Cargo, `tern` and `ya` on `PATH`; `/usr/bin/yazi` and `/bin/bash` are the real runtimes exercised by the fixture. `TERN=/absolute/path/to/tern` selects a specific Tern build.
- Active GUI display environment (`DISPLAY` or `WAYLAND_DISPLAY`). Tests use window-local control coordinates and do not switch the active desktop.
- Existing `libc` and `serde_json` dependencies only; no synthetic backend or snapshot generation.

`TERN_YAZI_TEST_WINDOW_HOOK=/absolute/path/to/executable` optionally places only the owned test window. The harness stops the renderer before it can create a window, invokes the executable with `<renderer-pid> <artifact-root> start`, then resumes that same process. During teardown (`Drop`), it invokes `<renderer-pid> <artifact-root> stop` before stopping the owned processes. A disposable hook can register PID-scoped placement before mapping and remove its own placement rule on `stop`; it must leave the active desktop, unrelated windows, and persistent desktop settings unchanged. The default harness needs no hook.

#### Execution commands

```sh
# Run the shell test runner
./tests/smoke_luau.sh

# Run the Rust integration interaction suite
cargo test --locked --manifest-path launcher/Cargo.toml --test interaction -- --nocapture
```

The suite drives mouse clicks, directory history, selection, forward/backward smart find and `n/N`, smart filtering and native Escape priority, breadcrumb/editor interactions, Tern file blocks and previews, foreground/background shell commands, yank/cut, exact Trash confirmation, reload, independent clients and shutdown. Assertions observe real Yazi snapshots, rendered UI, terminal output, filesystem effects and process identities. Missing runtime prerequisites fail instead of skipping. Successful runs remove their private fixtures; failures retain `failure.txt`, window logs and screenshots. Set `TERN_YAZI_KEEP_INTERACTION_ARTIFACTS=1` to retain a successful run for visual review. Coverage in the harness is not evidence that a fresh full-suite run passed; record the actual terminal result and inspected artifact paths separately.
### Native renderer plus a real Yazi fixture

The following setup uses the observed Tern `serve`/`ctl` interface and the fixture plugin loader. Execute it from a shell where `tern`, `ya`, and `yazi` resolve to the intended builds. Run the renderer and Yazi in separate foreground terminals so their lifetimes are explicit.

```sh
REPO=/absolute/path/to/tern-yazi
ROOT=$(mktemp -d /tmp/tern-yazi-qa.XXXXXX)
export ROOT
export TERN_CONFIG_DIR="$ROOT/config"
export XDG_RUNTIME_DIR="$ROOT/runtime"
export XDG_DATA_HOME="$ROOT/data"
export XDG_CACHE_HOME="$ROOT/cache"
export YAZI_CONFIG_HOME="$ROOT/yazi"
export STENCIL_FIXTURE_ROOT="$ROOT/crates/tern"
mkdir -p "$TERN_CONFIG_DIR" "$XDG_RUNTIME_DIR" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
mkdir -p "$YAZI_CONFIG_HOME/plugins" "$ROOT/crates/tern" "$ROOT/crates/plugins/fixtures"
mkdir -p "$ROOT/files/mixed/child" "$ROOT/files/sibling" "$ROOT/render"
ln -s "$REPO" "$ROOT/crates/plugins/fixtures/tern-yazi"
ln -s "$REPO/yazi-plugin/tern.yazi" "$YAZI_CONFIG_HOME/plugins/tern.yazi"
printf '%s\n' 'require("tern"):setup()' > "$YAZI_CONFIG_HOME/init.lua"
printf '%s\n' 'alpha' > "$ROOT/files/mixed/a.txt"
printf '%s\n' 'bravo' > "$ROOT/files/mixed/b.txt"
printf '%s\n' 'charlie' > "$ROOT/files/mixed/c.txt"
printf '%s\n' 'child entry' > "$ROOT/files/mixed/child/inside.txt"
printf '%s\n' 'hidden entry' > "$ROOT/files/mixed/.hidden.txt"
```

The temporary root is private and its path is shell-safe. All test files and Yazi configuration are disposable; this setup does not mutate `~/.config/yazi`.

Start the native renderer with those variables:

```sh
env -u TERN_PANE -u TERN_DAEMON -u TERN_SOCKET \
  TERN_CONFIG_DIR="$TERN_CONFIG_DIR" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
  XDG_DATA_HOME="$XDG_DATA_HOME" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
  STENCIL_FIXTURE_ROOT="$STENCIL_FIXTURE_ROOT" \
  tern serve --control "$ROOT/control.sock" --out "$ROOT/render"
```

Start ordinary Yazi in another terminal using the same root/XDG variables and sandbox config. Use the original executable; replace `/usr/bin/yazi` with the installation's `--real` path:

```sh
YAZI_CONFIG_HOME="$ROOT/yazi" XDG_RUNTIME_DIR="$ROOT/runtime" \
  XDG_DATA_HOME="$ROOT/data" XDG_CACHE_HOME="$ROOT/cache" \
  /usr/bin/yazi --client-id 987654323 "$ROOT/files/mixed"
```

In a control terminal, set the same `ROOT` and `STENCIL_FIXTURE_ROOT` values. Load the fixture and explicitly open the ordinary attached companion; allow its live actor probe to complete:

```sh
export STENCIL_FIXTURE_ROOT="$ROOT/crates/tern"
tern ctl --control "$ROOT/control.sock" plugins fixtures
tern ctl --control "$ROOT/control.sock" plugins run plugin.tern-yazi.toggle
tern ctl --control "$ROOT/control.sock" tree '[data-role="yazi.column.current"] .sf-item'
tern ctl --control "$ROOT/control.sock" shot initial
```

Screenshots are written under `$ROOT/render/serve/`, for example `initial.png`. The `tree` output gives native item identity and geometry; use coordinates from the current tree, not hard-coded positions after a resize. Its text output is truncated to 200 characters per node, so use a screenshot and independent fixture/file evidence for large-content assertions.

Observed control forms include:

```sh
tern ctl --control "$ROOT/control.sock" click X Y
tern ctl --control "$ROOT/control.sock" dblclick X Y
tern ctl --control "$ROOT/control.sock" key j
tern ctl --control "$ROOT/control.sock" key pagedown
tern ctl --control "$ROOT/control.sock" key home
tern ctl --control "$ROOT/control.sock" key shift+g
tern ctl --control "$ROOT/control.sock" key .
tern ctl --control "$ROOT/control.sock" key ';'
tern ctl --control "$ROOT/control.sock" key ':'
tern ctl --control "$ROOT/control.sock" type '"Mixed Case"'
tern ctl --control "$ROOT/control.sock" shot after-action
```

`X`/`Y` are the numeric coordinates reported for the current target row. The control CLI joins argv and parses scenario syntax internally: multiword `type` content needs scenario quotes in addition to shell quoting, as in the `"Mixed Case"` example. The shell's single quotes preserve those double quotes for the second parser. `type` inserts text; submit with Enter through the renderer or its key control. Focus the companion before keyboard scenarios. Avoid accidental `plugins run ...toggle` calls during coordinate-based tests: an existing pane switches layout and changes geometry.
### Managed launcher acceptance checks

These checks describe evidence to collect for the current managed design; they are not additional recorded smoke results. Keep the renderer, runtime inbox, Yazi configuration, launcher prefix, and test files disposable. Build/install with the real Yazi path and invoke the resulting prefix's `bin/yazi` from a shell pane in the isolated Tern window.

| Scenario | Required evidence |
| --- | --- |
| Build/install | Locked Cargo build succeeds; wrapper/helper/path/manifest are owned files; `launcher/target/` remains ignored |
| Outside Tern and informational options | Original executable receives the same arguments and exit behavior; help/version uses direct passthrough |
| Managed startup | Only a native browser block opens beside the launching shell; original Yazi has an invisible controlling PTY and generated client ID |
| Plugin missing or startup failure | Diagnostic describes early exit or the 15-second initial snapshot deadline; backend and owned runtime files are cleaned |
| Multiple launches | Different client/token pairs remain pinned; clicks, keys, replies, and confirmation affect only their own Yazi |
| Idle client and reload | 500 ms supervisor health and block-owned lease advance while snapshots deduplicate; reload saves bindings and verifies nonce-bound pane adoption |
| Closed/recycled pane during reload | The old client stops or gets a correctly pinned verified block; another client's pane never acquires its lease |
| `q`, Esc, and pane/owner close | Editing/confirmation/Esc priority remains intact; online Esc sends native escape without closing; managed closure terminates/reaps the backend and returns the shell |
| Window close/reopen | Persisted daemon block/owner panes retain the backend; the lease progresses while the window is closed, then the same client/token is restored |
| Lost health/lease | UI goes offline after 3 seconds without health progress; observed lease stops progressing and backend terminates after 8 seconds |
| Missing native attachment | Published managed startup ends after the 15-second attachment deadline |
| Ordinary attach and retained snapshots | Explicit attach requires an actor reply; old snapshots do not open panels or enable actions; closing attached UI preserves ordinary Yazi |
| Uninstall/ownership conflict | Same-prefix uninstall removes only owned unchanged files and retains original Yazi; unrelated/modified destinations produce an ownership error |


### Regression matrix

| Scenario | Required evidence |
| --- | --- |
| Click `c.txt`, then `j` | New Yazi snapshot hovers the clicked path, then its next sorted entry; native preview/highlight follow both moves |
| Parent and child single click | Yazi cwd becomes the target's containing directory and hovered URL is the exact clicked path |
| Directory/file double click | Directory changes cwd; file opener receives the exact path even when selection/old hover differs |
| External Yazi cursor/cd | Companion updates from snapshots without local input |
| Hidden toggle and smart filter | `.` changes Yazi's file list; `f` produces native smart-filtered rows (lowercase insensitive, uppercase sensitive); Enter retains, editing Esc clears; visual-mode Esc preserves the filter until a following Esc |
| Forward/backward smart find | `/`/`?` move native hover without hiding rows; `n/N` move next/previous; Esc clears find before a retained filter, and a following Esc restores the full listing |
| Breadcrumbs and path editor | Independent buttons navigate exact prefixes with hover feedback and full-prefix tooltips; ellipsis reveals ancestors; trailing single-line input accepts Unicode/relative/absolute paths, repeated clicks retain caret editing, Enter navigates and Esc cancels |
| Persistent/visual selection | Count/check marks remain independent of cursor; visual `+`/`-` marks follow range; Esc commits/leaves visual mode; `Space` toggles and advances |
| Yank and cut | `y` dispatches native yank copy; `x` dispatches native yank cut; Yazi clears selection automatically |
| Trash cancel | All fixture files and authoritative selection remain unchanged |
| Trash exact-target commit | Approved paths/client remain stable while external cursor/selection changes; only approved fixture files are trashed |
| Broken target/send/reply | Actual error displayed; queued follow-on requests cleared; no false completion claim |
| `;` non-blocking shell | Raw command submits native background task without opening a terminal; browser returns promptly; spaces and quotes survive |
| `:` blocking PTY shell | Raw command opens real visible PTY terminal displaying stdout, stderr, and exit code until Enter returns to browser; spaces and quotes survive |
| Directory metadata rendering | Current, parent, and child columns render strictly from authoritative snapshot `file_dirs` without filesystem directory enumeration; snapshot limits respected |
| Long lists and small pane | Current selection stays visible; local list wheel scrolling is contained; footer remains docked |
| Wide/tall raster and SVG | Preview is contained within the column; natural dimensions do not allocate column width |
| Markdown/code/Mermaid limits | Content within the configured limit renders; whole oversized files show a concise unavailable warning with the active limit, not a prefix, raw exception, traceback, `max_bytes`, or `host.luau`; a subsequent normal code/Markdown preview recovers |
| Live preview settings | Save a lower/higher valid limit without a cursor move; existing companion updates; malformed JSON shows `Preview settings unavailable` without raw exceptions, and correction recovers |
| Tern routing boundary | Plain PageUp/Down reaches the block; intercepted preset chords are recorded as upstream routing observations, not successful plugin paging |

Use disposable fixtures for shell, trash, yank state, openers, and extraction. For click/open tests, configure an isolated observable Yazi opener when an external application would make the result ambiguous; its marker must record the actual opened path. For asynchronous operations, inspect marker/task/filesystem outcomes separately from `ok` replies. For previews, create fixtures just below/above the configured thresholds and inspect both the rendered content and concise unavailable status, including normal code/Markdown recovery; a truncated tree string is not proof that content was truncated by the preview reader.

### Observed interactive results
The observations below are historical records from earlier isolated runs, not post-separation verification. Commands, complete terminal results and inspected screenshot paths from a fresh run must be recorded separately; a partial run does not establish a full-suite pass.

*Historical note (superseded)*: Earlier smoke work exercised an experimental custom manager-command parser (`: shell ...`) and older Esc/clipboard behaviors. Those trials are superseded by the current upstream Yazi 26.9.1 semantics. A later, previously recorded live-Yazi run observed the following restored native behavior:
- Physical `:` followed by `ls`, Enter printed `ls-visible.txt` with status 0 in a visible PTY terminal without any `shell` prefix, with clean selfprep status 0 and no lingering prep directories.
- Raw `;` input with `sleep 1; printf BackgroundDone > background-done.txt` remained in the browser, wrote the marker file via Yazi's background task, and created no shell handoff files.
- Native filter cascade: filtering `a.txt` followed by visual selection; first Esc transitions mode to `normal` while preserving `yz.filter = "a.txt"`; second Esc sets `yz.filter = false` and restores the unfiltered listing with the filter bar closed.
- Native yank and cut: `y` copy generates native duplicate targets, `x` cut removes the source entry and preserves content at the destination; selection is cleared natively without frontend clipboard involvement.
- Native history: `H` (back) and `L` (forward) traverse directory history natively after an explicitly revealed child.
- These previously recorded outcomes do not establish the status of a fresh full-suite run.

On the runtime versions above, isolated real-Yazi/native-renderer smoke work exercised current-column click followed by `j`, parent/child single clicks, directory/file double clicks, hidden toggle, raw `;` shell input with a Unicode marker, and a historical `:` shell trial. Confirmation smoke separately observed cancellation without file mutation and exact-target trash dispatch.
Historical preview smoke rendered 62,463 bytes through the native code renderer and a 63,848-byte Markdown fixture through its final heading (`h-full-markdown-end`). That older presentation exposed a `max_bytes` error after saving a 1 KiB code limit; current preview failures use the concise warnings described above. Restoring 1 MiB recovered the preview without a cursor move. A configured code limit of `999999999` clamped to 4 MiB, and a 5 MiB fixture was rejected. The settings command initialized the actual sandbox store at `$ROOT/config/plugin-data/tern-yazi/kv.json`. The decoded PNG/small SVG and the large-SVG limitation are recorded in the preview section. These historical observations do not turn every row of the regression matrix into a passed automated test.

A 17 MiB image was rejected at the 16 MiB read limit, while the 10.3 MiB PNG decoded. A roughly 45 KiB Mermaid fixture rendered a native `.mfig-body.mmd` SVG measuring 165 by 40 pixels. These are observed preview results, distinct from the registration-only script; they do not assert that the final coordinated reload repeated all earlier click/shell regressions.

### Historical managed startup results

The isolated native-window run at `/tmp/tern-yazi-native-start-w7Al3K` exercised the installed local `yazi` wrapper with real Yazi 26.9.1. Typing `yazi` opened a native companion beside the launching shell while Yazi remained in an invisible PTY. A click changed the authoritative snapshot to `a.txt`, followed by `j` to `b.txt`. Two clients kept separate cwd/hover state; closing one left the other alive. Toggling from the launching shell reused its existing managed panel. Plugin reload preserved the client/token/pane binding.

An idle 12-second interval advanced the host-owned lease by approximately 11.85 seconds. Closing the window for 12 seconds also advanced the lease while the daemon retained its panes; reopening the retained session restored the same client/pane. Different window instances produced different UUID-based acknowledgement nonces. Tern CLI commands are scoped by `TERN_WINDOW_KEY`; reopening another window creates its own scope, so select the retained session rather than assuming all windows share a default tab.

The final context/resize smoke matched native pane and lease dimensions at 45 rows by 78 columns, then 35 rows by 60 columns after resizing the window. A subsequent 10-second idle interval retained the live backend. The installed wrapper's `q` path then removed its session files.

`q`, Esc, native-pane close, and owner-pane close removed their managed runtime files. Killing the actual Yazi child while local shell input was active produced the offline surface; one `q` closed it without recreating session files. A raw-shell operation wrote `NativeUmask` to a disposable file with mode `0644`, preserving the launching shell's umask. Missing Yazi plugin setup produced the 15-second initial-snapshot error, an ANSI-stripped bounded diagnostic, exit status 1, and an empty session inbox.

Outside-Tern and inside-Tern `--version` invocations ran original Yazi 26.9.1. Installation rejected an unrelated wrapper, uninstall rejected a modified owned wrapper, and same-prefix sandbox uninstall removed the four owned files while preserving the original executable's SHA-256 digest. `cargo fmt --check` and `cargo clippy --locked -- -D warnings` passed. Native screenshots and layout evidence remain under `target/shots/tern/live/` inside the disposable root, including `host-activated-live.png`, `multi-clients-reload.png`, `installed-stable-live.png`, and `installed-resized-live.png`.


### Historical native zoom overlay and terminal shell results

In the live Yazi 26.9.1 verification environment, typing `yazi` launched directly into a native visual zoom overlay that preserved the owner shell and split tree.

Interactive shell execution verified:
- PTY terminal execution displayed stdout `ShellVisible`, stderr `ShellError`, exit status 7, and retained error status 9 in the visible terminal pane.
- Interactive stdin read Unicode text `Hello 阿尔帕卡` correctly.
- Blocking shell terminal handoff verified (historical trial tested via `: shell` script `--block`; current behavior uses raw `:` without `shell` prefix): output and working directory (`cwd`) were correct.
- Pressing Enter removed the status 9 terminal pane and returned directly to the same live Yazi session in the zoom overlay.
- `q` and reload followed by `q` cleaned the runtime inbox and restored the owner shell unzoomed.
- Dedicated path card geometry was observed at 331px wide by 28px tall, the separate status badges row at 18px tall, column headers at 28px tall, and full-path `title` tooltips were verified.

Final protocol smoke verified:
- `pwd` matched authoritative files `cwd`.
- Final stderr output remained visible and status 4 was visible in the PTY terminal.
- Pressing Enter returned to the same client with the original unzoomed state and all shell handoff files removed from the inbox.
- `$0` and `$@` both resolved to the authoritative hovered child when no selection was active.
- Existing original 2-pane split tree (id `579820584962`, `axisRow`, `ratio` 0.5) with exact `a`/`b` pane IDs was restored after launch and `q`; the sibling pane remained alive and zoom was false. Custom split ratios, pre-zoomed restoration, and forged challenges remain unexercised.
- Final screenshot evidence was inspected under `target/shots/tern/live/visual-overlay-final.png` and `path-box-narrow-final-clear.png`.

Stop the foreground Yazi and renderer before removing the exact disposable root. Retain screenshots/logs only when needed as regression evidence; do not leave QA fixtures in the repository or reuse the live user's Yazi configuration.

### Historical pre-separation native interaction verification (2026-10-10)

Before the frontend/backend module separation, the complete real interaction suite passed with Tern `0.7.0 (9ca00e4)`, Yazi `26.9.1 (Terra 2025-12-27)` and Cargo `1.96.0 (30a34c682 2026-05-25)`. The following recorded command chain exited with status 0. This evidence is retained unchanged as the pre-refactor baseline; it does not establish a post-refactor runtime pass:

```sh
cargo fmt --manifest-path launcher/Cargo.toml && env TERN_YAZI_TEST_WINDOW_HOOK=/tmp/tern-yazi-background-window.sh TERN_YAZI_KEEP_INTERACTION_ARTIFACTS=1 ./tests/smoke_luau.sh && cargo clippy --locked --manifest-path launcher/Cargo.toml --all-targets -- -D warnings && cargo fmt --manifest-path launcher/Cargo.toml --check && sh -n tests/smoke_luau.sh install.sh && luajit -b yazi-plugin/tern.yazi/main.lua /tmp/tern-yazi-final-main.luac
```

The interaction result reported:

```text
All real native interaction cases passed (owned GUI, isolated daemon).
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 43.13s
```

The retained proof root is `/tmp/tyi-2095558-18dd2308f81eb4f2`; native screenshots are under its `target/shots/tern/live/` directory. The disposable placement hook assigned only the owned PID's window to desktop 1 without switching the active desktop. Screenshots were produced with that owned window's `ctl shot`, not a desktop capture.

Observed assertions and inspected screenshots:

- `02-preview-limit-no-traceback.png` shows the sparse text fixture of 1 MiB + 1 byte with only `Preview unavailable (1.0 MB limit)`. The unavailable status contains no raw runtime exception, traceback, `max_bytes` or `host.luau` text. Subsequent normal text and Markdown previews recovered.
- `04-breadcrumb-hover.png` was inspected, and the actual native `:hover` selector assertion passed for the current `files` breadcrumb with its full-prefix `title`. Breadcrumb clicks, repeated-click caret editing, Unicode long paths, narrow layouts, ancestor ellipsis navigation, non-directory rejection and recovery passed.
- Forward/backward native smart find, `n/N`, smart filtering and native Escape priority passed against real Yazi snapshots.
- Single and selected case-distinct batch Tern file opening passed with exact targets and focus assertions.
- Raw `:` with `ls`, visible stdout/stderr and exit status, interactive stdin, native macros and non-blocking background shell execution passed. Native copy/cut and exact-path Trash assertions passed.
- Plugin reload and independent clients passed. `q`, native pane close and owner close cleaned owned descendants; crashed Yazi rendered offline and `j`/Enter/`o` opened no files. `q` and reload did not resurrect closed controls.

That pre-separation run establishes the listed native GUI outcomes for its checkout, not a post-refactor pass, successful routing of intercepted physical Ctrl+D/U/F/B or Shift+PageUp/Down chords, or new proof for the historical giant-image/SVG decoder boundaries.

### Post-separation native interaction verification (2026-10-10)

The parent verification run exercised native relative module loading and the existing real interaction scenarios through the separated frontend/backend implementation. The test harness and scenarios were unchanged. This actual command chain exited with status 0, including the GUI suite, strict all-target Clippy, formatting check, shell syntax checks, and Yazi Lua compilation:

```sh
env TERN_YAZI_TEST_WINDOW_HOOK=/tmp/tern-yazi-background-window.sh TERN_YAZI_KEEP_INTERACTION_ARTIFACTS=1 ./tests/smoke_luau.sh && cargo clippy --locked --manifest-path launcher/Cargo.toml --all-targets -- -D warnings && cargo fmt --manifest-path launcher/Cargo.toml --check && sh -n tests/smoke_luau.sh install.sh && luajit -b yazi-plugin/tern.yazi/main.lua /tmp/tern-yazi-refactor-main.luac
```

The exact interaction result was:

```text
All real native interaction cases passed (owned GUI, isolated daemon).
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 42.90s
```

The retained proof root is `/tmp/tyi-2249097-18dd25463b1c69d0`; native screenshots are under `target/shots/tern/live/` inside that root. The configured disposable window hook places only the owned GUI window on desktop 1 without switching the active desktop. Screenshot files include `02-preview-limit-no-traceback.png` and `06-real-colon-ls.png`; their presence is not, by itself, a claim of visual inspection.

Exercised outcomes include:

- Cold/idle managed startup; breadcrumbs, UTF-16 path editing, native smart-case filter/find, and Escape ordering.
- Native previews, concise oversized-file failure and recovery, and exact selected-file batch routing.
- Blocking native shell stdout/stderr/stdin and macros, non-blocking background shell, copy/cut, and exact-target Trash.
- Reload, independent clients, crash/offline behavior, and process/control-file cleanup.

The final filter-ownership adjustment is included in this run: local editing or pending filter mutations retain the local query, but unrelated escape/action acknowledgements no longer block synchronization from the authoritative filter snapshot. A parent source-boundary scan also found no `tern.fs`/`tern.process` calls in the frontend, and no native UI/node, context/presentation, layout, or session-effect APIs in the backend. Backend semantic language/MIME lookup tables are initialized once per VM; their public APIs are unchanged.

This post-separation pass does not establish routing for the known intercepted physical paging chords or new giant-image/SVG decoder behavior beyond the exercised scenarios. The earlier evidence above remains a separate historical baseline.

## 10. Contribution checklist

- Identify which side owns the change: external Yazi state/actors or Rust supervision, native frontend UI/input/layout, backend snapshot/IPC/liveness/preview data, or pure shared helpers. Keep one authoritative source for each behavior and preserve the frontend -> backend -> shared dependency direction.
- Inspect the installed API declarations and Yazi executor before adding an action or option. Update request encoding and decoding together; preserve actor restrictions and exact-target validation.
- Match existing Luau/Lua indentation and identifiers; use English comments/documentation. Keep changes scoped to the requested behavior.
- Preserve stable native node keys, list IDs, selected-item semantics, and surface-scoped CSS. Test actual geometry after changes to list/preview containers.
- Add a concrete offline/registration or isolated interactive regression appropriate to the change. Do not treat a ready plugin registration as a rendered-UI test, and do not claim asynchronous completion from an acknowledgement.
- Update this guide and the minimal README when the public installation, commands, configuration, or outstanding feature set changes. Preserve the intentional ignored local planning/documentation paths.
- Add no dependency or persistent user configuration change without agreement. Keep one-off scripts and fixtures outside the checkout.
- Record only verification actually exercised, including runtime versions and upstream routing boundaries. Do not commit or push as an incidental development step.
