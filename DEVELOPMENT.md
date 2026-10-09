# Development Guide

This document describes the native `tern-yazi` implementation: a Tern Luau block and window plugin, plus a Yazi Lua plugin. Yazi owns file-manager state and executes manager actions; Tern owns the companion's layout, native previews, input bars, and confirmation layer. The companion does not launch Yazi or run a Python process.

## 1. Environment and installation

### Runtime reference

The current Linux interactive verification environment uses:

| Component | Observed version | Responsibility |
| --- | --- | --- |
| Tern | `0.7.0` (`9ca00e4`) | Host/window plugin VMs, native block UI, filesystem/process APIs, layout, input routing |
| Yazi | `26.9.1` | Authoritative directory, cursor, selection, visual mode, and task state |
| Ya | `26.9.1` | `ya emit-to` dispatch to the chosen Yazi client |

These are observed versions, not a compatibility matrix. The generated declarations in [`tern.d.luau`](tern.d.luau) describe the Tern APIs used by this checkout. Both Yazi and the Tern host must run on the same machine and see the same runtime inbox. Keep `ya` and `tern` on the host process's `PATH`; Terminal Here invokes `tern` as a subprocess.

The native plugin has no package installation or Python dependency step. A Nerd Font supplies the file-type glyphs. Archive paths additionally use installed `unzip`, `tar`, or `7z` executables; listing support and extraction support differ by format.

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
| [`host.luau`](host.luau) | Snapshot selection, block state, view construction, native list event handling, keyboard actions, serialized requests, previews, confirmation layer |
| [`window.luau`](window.luau) | Pane adoption/creation, floating and docked layouts, commands, global shortcut, snapshot/configuration polling |
| [`companion.css`](companion.css) | Three-column geometry, pane-contained list/preview scrolling, image containment, footer and confirmation styling |
| [`yazi-plugin/tern.yazi/main.lua`](yazi-plugin/tern.yazi/main.lua) | Yazi telemetry hooks, atomic snapshots/replies, request decoding, actor dispatch, target selection guard |
| [`tern.d.luau`](tern.d.luau) | Generated native Tern type and API contracts |
| [`tests/smoke_luau.sh`](tests/smoke_luau.sh) | Plugin registration/reload smoke script with a synthetic snapshot |

There is no supported Python entry point or Python test suite in this checkout. Local bytecode or ignored development artifacts are not part of the native runtime. The root `DEVELOPMENT.md` is the published developer guide; the intentionally excluded `/AGENTS.md`, `/PLAN.md`, and `/docs/` remain local and are not installation prerequisites.

## 3. Architecture and lifecycle

```text
Yazi manager state
    |
    | hover / cd / Status redraw hooks
    v
tern.yazi/main.lua -- atomic state-<client-id>.json --> runtime inbox
                                                          |
                                             window.luau polls every 100 ms
                                                          |
                                              main.poll action / new block
                                                          v
                                                host.luau companion view
                                                          |
                         serialized ya emit-to <client-id> plugin tern <JSON>
                                                          |
                                                          v
                                               tern.yazi request executor
                                                          |
                              atomic reply-<request-id>.json + next snapshot
                                                          |
                                                          v
                                              host consumes reply; next job
```

### Yazi setup and snapshot production

`M:setup()` creates the inbox directory, subscribes to Yazi's local `hover` and `cd` events, registers the remote `tern-cmd` topic, and installs a `Status:children_add(..., 1000, Status.RIGHT)` callback. Local event bodies contain a tab reference rather than a complete state; the plugin reads the settled `cx` state directly in synchronous callbacks.

Hover and directory callbacks publish DDS telemetry, pulse selection/tasks, and call `dump_state()`. Status redraws provide the missing task-progress trigger and also call `dump_state()`. Task totals come from `cx.tasks.summary`; byte counters are summed from `cx.tasks.snaps[*].prog`. The plugin does not use `cx.tasks.progress`.

Snapshot deduplication covers cwd, current filenames, hovered URL, selection count and paths, visual mode and marked paths, and task counters. An unchanged composite key does not write another file. Metadata such as size/mtime and the parent listing are included in snapshots but are not independent deduplication triggers. A metadata-only update therefore need not immediately produce a new snapshot. An explicit request's `snapshot()` clears the deduplication key before dumping.

Writes use `state-<client-id>.tmp`, close the file, and rename it to `state-<client-id>.json`. Readers never intentionally consume the intermediate filename. `seq` increments per emitted snapshot; `ts` uses `ya.time()` when available, otherwise `os.time()`.

### Window lifecycle

The window half tracks one companion pane reference and a last-observed snapshot score. `window_start` calls `poke()`, and a self-rearming 100 ms timer keeps calling it. Each tick independently checks preview configuration and scans snapshot files. A greater snapshot score sends `{ev="action", id="main.poll", act="poll"}` to the live pane, or opens the companion when none is live.

Opening first adopts an existing pane whose block is `tern-yazi.companion`. Otherwise `cx:new_block(..., "beside", {focus=false})` creates one and floats it in the top-right corner. The global `Ctrl+Alt+Y` shortcut and `Yazi: Toggle Companion Panel` command (`plugin.tern-yazi.toggle`) switch a live pane between floating and docked layouts; they create a pane if necessary. This is a layout toggle, not a persistent hide/show setting. `pane_closed` clears the tracked reference. A later newer snapshot can reopen a closed companion.

### Block lifecycle

`tern.block.define("companion", ...)` supplies `init`, `view`, `title`, `event`, `key`, and `save` callbacks. Initialization creates transient local input/optimistic state and an archive cache. `save()` returns `nil`; local command/filter editing, pending confirmation, and cache state are not persisted by the block.

Every view reads the latest valid snapshot. Without one, the block renders a waiting card and Check Again action. With one, it renders a path/selection header, optional filter/command/task rows, three Miller columns, a dock footer, and an optional confirmation layer. A `poll` action causes the view to be rerun without a separate state-reducer operation. `Ctrl+R` also requests a normal re-render; it does not fetch a new snapshot from Yazi.

There is no active-client picker or focus-based client binding. The host scans all `state-*.json` files and chooses the largest positive `ts`, falling back to `seq` when `ts` is absent/nonpositive. The client ID is taken from the filename, overriding its JSON member. Both host and window snapshot reads have a 256 KiB cap; unreadable/invalid host snapshots are skipped. A very large directory or selection can exceed that cap.

Snapshot files are not expired or removed on Yazi exit. A stale snapshot can still render. The `Synced` badge is a presentation label, not a liveness probe. Window polling uses a greater-than comparison; restarting a legacy producer with only a lower `seq` is not equivalent to publishing a newer timestamp.

## 4. Inbox and wire protocol

### Runtime location and trust boundary

All three native participants resolve the inbox as:

```text
$XDG_RUNTIME_DIR/tern-yazi
```

If `XDG_RUNTIME_DIR` is unset or empty, they use `/tmp/tern-yazi`. Use the same value in the Tern host/window and Yazi processes. Use a trusted, user-private runtime directory. The inbox contains local paths and selection metadata in plaintext; it is not an authenticated protocol or a sandbox. The plugin does not enforce ownership/private permissions on an existing directory. Yazi currently creates the directory through an unquoted `mkdir -p` command, so the runtime path must not contain whitespace or shell metacharacters.

### Snapshot schema

A representative snapshot is:

```json
{
  "ts": 1791580000.25,
  "seq": 42,
  "client_id": "987654323",
  "cwd": "/tmp/example",
  "files": ["a.txt", "child"],
  "parent": {"cwd": "/tmp", "files": ["example", "other"]},
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

`files` preserves Yazi's filtered/sorted current listing; persistent `selected_urls` are sorted absolute paths. `mode` is `normal`, `select`, or `unset`. `marked_urls` carries visual-range marks, separate from persistent selection. Hover and parent objects can be absent; task reads fall back to zero counters if their protected read fails. The Yazi parent snapshot contains at most 30 entries. `client_id` is read from Yazi's `YAZI_ID`, with `default` as its fallback; explicit `yazi --client-id` launches are preferable for regression fixtures.

### Request serialization

The host maintains a FIFO queue and sends one request at a time. Each request freezes its destination client at enqueue time. IDs combine an epoch derived from `tern.now()`, the pane identifier, and an incrementing counter. The Yazi executor validates IDs against `^[%w-]+$` before constructing reply paths.

The process invocation is an argv array:

```text
ya emit-to <client-id> plugin tern <quoted-request-json>
```

The JSON must additionally be quoted for Yazi's plugin-argument parser. `pump()` wraps the encoded JSON in single quotes and escapes embedded apostrophes independently of process argv boundaries. Keep this second parsing layer when changing serialization.

Mixed Lua tables cannot safely encode both positional and named arguments as a JSON array. The host splits them into explicit wire members; Yazi reconstructs numeric and option keys before calling its actor:

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
| `action` | `action`, `args` | Allows `arrow`, `cd`, `reveal`, `visual_mode`, `escape`, `hidden`; executes actor and emits a forced snapshot |
| `command` | `action`, `args`, optional `target`/`cwd` | Validates the manager actor and restrictions; dispatches normally or translates to a target-aware operation |
| `open` | `target`, `cwd` | Opens an explicit `Url(target)` through Yazi with `Url(cwd)`; does not substitute Yazi's current selection |
| `toggle` | `target` | Resolves the exact file, toggles it through Yazi, and snapshots |
| `filter` | `query` | Calls `filter_do` with `insensitive=true`, `done=true`, and snapshots |
| `yank` | `target` | Computes selected/marked-or-fallback targets, enters the guarded exact-selection pipeline, and returns the same paths |
| `trash_prepare` | `target` | Returns authoritative target paths without mutating Yazi or the filesystem |
| `trash_commit` | `paths` | Validates each file and runs the guarded confirmed-trash pipeline |

### Replies and completion meaning

Yazi writes a reply to `reply-<id>.json` by writing/closing a `.tmp` file and renaming it. Replies contain `id` and `ok`, plus `error` on failure, or operation-specific `paths`, `queued`, and `confirm_trash` fields.

A successful `ya` exit only proves transport dispatch. After it exits successfully, the host polls the reply file immediately and then every 50 ms, up to 200 attempts (approximately 10 seconds). A matching reply ID is consumed and its file removed. The next queued request begins only after the current one finishes. A failure clears the remaining queue and optimistic cursor/cwd, and displays the actual error. Spawn errors, nonzero transport exits, invalid/missing replies, and timeout are not reported as success.

An `ok` reply acknowledges actor acceptance or the target pipeline's settled dispatch, not completion of an opener, shell process, trash job, or filesystem task. Inspect Yazi task results and fixture files for end-to-end outcomes. Timeout explicitly means the outcome is unknown; do not blindly retry a destructive request. Late/unconsumed replies can remain in the inbox.

### DDS telemetry is a separate interface

The Yazi plugin still publishes `tern-hover`, `tern-cd`, and deduplicated `tern-state` telemetry via `ps.pub_to(0, ...)`. It registers `ps.sub_remote("tern-cmd", ...)`: `{"op":"hello"}` republishes folder/hover telemetry, while other bodies produce a `tern-ack` echo, followed by a pulse. This capability is necessary for inbound custom DDS kinds in Yazi 26.9.

The native companion does not run `ya sub` or use this DDS stream as its view transport. Its state comes from snapshot files, and its command requests enter `M:entry()` through `ya emit-to ... plugin tern`. Do not confuse DDS echo acknowledgements with per-request actor replies.

## 5. UI state and event wiring

### Authoritative versus optimistic state

Yazi snapshots own the current directory listing, cursor, persistent selection, visual marks, and tasks. The host retains only local editing state, a provisional `active_file`/`optimistic_cwd`, pending trash paths, request queue, and archive cache.

`sync_hover()` clears optimistic state when the source client, cwd, or hovered URL changes. A task-only or selection-only snapshot with those three values unchanged must not discard a click that is still being dispatched. This distinction is important for the click-then-keyboard regression: a click requests an actual Yazi `reveal`, and later `j`/`k` moves from that real cursor, not from a Tern-only highlight.

Current-column names come directly from `yz.files` when rendering the authoritative cwd. Only an optimistic different cwd uses a local directory listing. Filtering is performed by Yazi; the companion does not maintain a second local regex filter. Parent data uses the snapshot when available, otherwise a live listing; child contents use a live sorted `tern.fs.list`. Those live columns need not share Yazi's active filtering policy.

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
| Any list `activate` on a directory | Provisional cwd becomes the target; send `cd(target)` |
| Any list `activate` on a file | Send explicit-path `open(target, cwd=base)` to Yazi |

The handler accepts both `select` and `activate` using the item's semantic ID. It must not interpret a native item string as a numeric index or activate whichever file Yazi happened to hover before the click.

Green check marks represent persistent selected paths. The native selected-row capsule represents cursor position. Amber `+`/`-` marks represent the visual range (`select`/`unset`); these are not persistent selection marks. Counts remain based on Yazi's snapshot while the cursor moves.

### Regions, layout, and actions

The block returns `main`, `dock`, and `layer` regions. Region roots are stripped by Tern, so the footer row is wrapped as a child of a `ui.col` in `dock`. The confirmation lives in `layer` with its own opaque card and pointer-enabled controls.

CSS assigns parent/current/preview shares of `4fr / 9fr / 7fr`, each with `minmax(0, ...)`. Pane height is inherited through flex/grid containers with `min-height: 0`; only native list scrollers and the preview region scroll. Lists retain `max.lines` and full selected node IDs for native selected-row reveal. Do not pad lists to terminal rows or remove the native selection-reveal property. Image wrappers constrain natural raster/SVG dimensions and use `object-fit: contain`.

Clickable controls use action strings such as `open_file=<path>`, `copy_path=<path>`, and `terminal_here=<cwd>`, delivered to the handler as `ev.act`/`ev.value`. `event()` also accepts a structured `shortcut` action carrying `ev.key`; both that route and the block's `key()` call the same `handle_key()` implementation.

File-preview buttons open through Tern, copy the visible path directly, launch `tern split down --cwd <cwd>`, or extract the visible archive. The keyboard `y` shortcut instead performs authoritative Yazi yank and copies its returned target set; the Copy Path button copies only its visible path.

## 6. Keyboard and local command semantics

### Normal mode

| Key | Behavior |
| --- | --- |
| `j` / Down, `k` / Up | Yazi `arrow` by `1` / `-1` |
| `h` / Left / Backspace | `cd` to the visible cwd's parent |
| `l` / Right | Enter a visible directory; no file open |
| Enter | Enter a directory or request explicit-path file open through Yazi |
| Space | Toggle the visible exact path in Yazi's persistent selection |
| `v` | Yazi `visual_mode` |
| `y` | Yank authoritative selected/visual-or-visible targets; copy those paths to the clipboard |
| `o` | Open the visible path through Tern |
| `d` | Prepare target paths and show native trash confirmation |
| `g` / Home | `arrow top` |
| `G` / End | `arrow bot` |
| PageDown / PageUp | `arrow 100%` / `-100%` (Yazi viewport units) |
| Shift+PageDown / Shift+PageUp | Half-page semantic handler, subject to Tern routing below |
| Ctrl+D / Ctrl+U | Half-page semantic handler, subject to Tern routing below |
| Ctrl+F / Ctrl+B | Full-page semantic handler, subject to Tern routing below |
| `/` | Edit a live case-insensitive Yazi regex filter |
| `;` | Edit a raw non-interactive shell command locally |
| `:` / Shift+`;` | Edit a manager action and its arguments locally |
| `.` | Yazi `hidden` with `state="toggle"` |
| Ctrl+R | Re-render from the available snapshot |
| `x` | Extract the visible archive into the visible cwd; not Yazi cut |
| `q` | Exit the companion block |
| Esc | Cancel input/confirmation, clear retained filter, commit/leave visual mode, or exit the block, in that priority order |

Shifted letters other than `G` are not silently treated as their unshifted actions. Normal-mode Alt/Meta chords are not claimed by the handler.

### Input editing

Both input bars accept printable key text, Space, paste, and Unicode. Backspace removes one UTF-8 codepoint, not an entire grapheme cluster. Enter submits the current text; Esc cancels locally. The command bar remains available after a submission error. A successful reply clears the bar only if its text still matches the submitted text.

Filter editing sends `filter` requests as the text changes. Enter leaves editing while retaining the filter. Esc/clear sends an empty query and clears local filter state before a later Esc can exit the block.

The `;` bar sends the raw string as the shell actor's first positional argument with the visible cwd as an option. It does not parse shell words, rewrite quoting, or open an extra Yazi shell prompt. Shell syntax, substitutions, and side effects belong to the shell/Yazi execution environment; this is not a safe shell sandbox.

The `:` bar is a manager-command parser, not a shell. It supports whitespace-separated words, single/double quotes, and backslash escapes outside single quotes. Unclosed quotes and trailing escapes fail locally. It performs no glob expansion, variable expansion, substitution, or piping. `--name` becomes boolean `true`; `--name=value` becomes a string option; hyphens in option names become underscores. Other words remain positional strings. For example:

```text
:cd '/absolute/path/with spaces'
:hidden --state=toggle
:shell 'printf "marker\n" > result.txt' --orphan
```

In the last example, only Yazi's shell actor interprets the quoted shell program. The manager parser itself does not interpret the redirection. Raw `;` commands are the simpler route when submitting shell syntax.

Accepted manager action names are:

```text
cd arrow leave enter back forward reveal follow stash open yank unyank
 toggle toggle_all visual_arrow visual_mode escape copy shell hidden
 linemode filter filter_do sort refresh quit close suspend seek remove
```

The allowlist is tied to the Yazi 26.9.1 manager executor. It is not arbitrary plugin execution. Interactive options are rejected to avoid opening a second Yazi prompt. `remove` is translated into native confirmation and rejects `--force`/`--permanently`; `yank --cut` is rejected. `toggle`, argument-free `open`, `yank`, and filter actions use the companion's target-aware paths. Other supported actions pass reconstructed arguments to `ya.exec`.

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

Values are positive, finite integer byte counts. Missing/invalid fields use their category defaults; values above the category maximum are clamped. A missing store is empty. Invalid JSON or an unreadable store raises a settings error instead of silently applying a different configuration.

`tern.kv.get` rereads the file when it changes on disk, shared across the plugin's host/window VMs. The window's 100 ms configuration-stamp check sends a normal `poll` event to an existing/adopted companion when the encoded store value or error changes. The host resolves limits on each preview build, so saving a corrected configuration retries the read without needing a Yazi cursor move or source reload. These are plugin-private settings, not global Tern preferences.

`tern.fs.read(path, limit)` rejects a whole oversized file before allocating/reading its content. Its API requires a regular non-symlink target, bounds raced growth, and avoids blocking opens of nonregular targets. It does not return a truncated prefix. Read failures show the active limit and actual filesystem error; settings failures show a settings error. Preview byte caps do not modify image geometry or guarantee bounded rendered DOM complexity.

The image cap matches Tern's documented [16 MiB single-blob limit](https://docs.stencil.so/tern/protocol/operations.html#security-and-limits). Successful bounded reads and blob creation do not guarantee that an image decoder accepts the content. In the observed runtime, a 10,837,739-byte PNG decoded into native image/zoom UI, and a 183-byte SVG decoded; a roughly 9 MiB SVG dominated by an XML comment remained a missing-image placeholder. Treat that SVG case as a renderer boundary rather than a plugin read-limit failure. Image byte settings cannot override native decoder/parser limits.

Rendering dispatch is extension-based:

- Images/SVG use bounded raw bytes, `cx:blob(content, mime)`, and a native `image` node. Empty images and blob failures get explicit messages.
- Mermaid is wrapped in a Mermaid Markdown fence and passed to `ui.md`.
- Markdown uses `ui.md(content)`.
- Other files with NUL bytes show a binary-file message; otherwise `ui.code(content, language)` applies the extension language map, falling back to `text`.
- A hovered directory uses a child-column list from the local filesystem, not a byte preview.
- Archives use asynchronous external listing rather than `read_preview`: ZIP uses `unzip -Z1`; tar-like formats use `tar -tf`; `.7z` uses `7z l -ba -slt`; other recognized archive extensions fall back to tar. At most 50 output lines are cached and at most 18 displayed. This is a bounded line display, not a full archive-entry parser. The URL-keyed cache does not invalidate on archive modification; an async result is visible on a subsequent render.

There is no dedicated PDF, audio, or video viewer. Archive extraction uses `unzip -o ... -d <cwd>` for ZIP and `tar -xf ... -C <cwd>` otherwise. Extraction runs through Tern, not the Yazi request queue or trash confirmation. It can overwrite files and has no native confirmation/undo; exercise it only with disposable trusted fixtures. Recognizing an extension for listing does not imply extraction support.

## 8. Exact-target safety and regressions

### Target calculation

`targets()` starts with Yazi's persistent selection, adds visual marks with `is_marked()==1`, removes marks with `is_marked()==2`, and sorts the resulting absolute paths. Only if that result is empty does it fall back to the visible target. This same authoritative calculation underlies yank and trash preparation.

### Two-phase trash

1. `trash_prepare` returns target paths without any mutation.
2. The host freezes those paths and their originating client in `state.trash`, and displays every exact path JSON-encoded in the native confirmation layer.
3. While that layer is active, keyboard and UI actions are restricted to confirm/cancel. Cancel clears local confirmation and sends no mutation.
4. Confirm sends `trash_commit` to the frozen client, even if snapshot selection has switched to another client.
5. Yazi resolves every approved path through `fs.file(Url(path))` before committing.
6. A synchronous stage clears visual/selection state and toggles the approved files on. It queues the `finalize` plugin stage with `mode="sync"` behind those toggles.
7. `finalize` compares normal-mode state and the entire sorted authoritative selection to the approved paths. Any mismatch refuses the operation.
8. Only after that guard does it emit `remove` with `force=true` and no permanent-removal option. Here `force` skips Yazi's second confirmation; it does not request permanent deletion.
9. A following `settled` stage snapshots and acknowledges dispatch. Task/filesystem results still determine eventual completion.

The same guarded exact-selection pipeline is used for yank, followed by clipboard copying of returned paths. `self.pending` rejects another overlapping target operation. The synchronous FIFO guard matters because Yazi synchronous emits preempt ordinary queued commands; an unrelated reveal must not retarget the final actor.

### Preserve these invariants

- A single click updates the real Yazi cursor through `reveal`; the next keyboard movement starts there.
- File activation opens the explicit clicked path with its containing cwd, not an old hovered or selected file.
- Task-only snapshots do not erase provisional click state.
- Current rows follow Yazi's sorting/filtering and hidden-state toggle rather than a parallel local filter.
- Native list item IDs and `args.positional` numeric reconstruction remain intact through the wire.
- Raw shell input preserves spaces, Unicode, quotes, and shell syntax; manager input preserves its separate quoting/option semantics.
- Cancelled confirmation has no Yazi/filesystem mutation. Approved paths/client remain frozen until commit.
- Transport success, actor acknowledgement, and asynchronous task completion remain distinct claims.
- A preview failure reports the actual cause without truncating content or relaxing caps.

## 9. Reproducible isolated QA

There are two different verification layers. Plugin registration verifies manifest/VM loading. Interactive fixtures verify events, layout, actor effects, and rendered previews. Record the commands, versions, expected outcomes, and observed files/screenshots for the layer actually exercised.

### Registration/reload smoke

The checked-in script links the plugin under a disposable `TERN_CONFIG_DIR`, verifies `ready` in `plugin list`, writes a synthetic `state-9999.json`, reloads, checks registration again, and unlinks. It does not open a companion block, start Yazi, dispatch clicks/keys, or assert rendered pixels. Its synthetic snapshot should not be treated as a complete protocol fixture.

Run it with a dedicated runtime inbox as well:

```sh
runtime=$(mktemp -d /tmp/tern-yazi-smoke-runtime.XXXXXX)
XDG_RUNTIME_DIR="$runtime" sh tests/smoke_luau.sh
```

The script removes its own temporary configuration and `state-9999.json`; inspect and remove the remaining disposable runtime directory after it exits. Do not run it against the working session's inbox: the fixed filename could collide with a real client. `TERN_CONFIG_DIR` alone does not isolate a daemon/window or the runtime inbox.

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

Start Yazi in another terminal using the same root/XDG variables and sandbox config:

```sh
YAZI_CONFIG_HOME="$ROOT/yazi" XDG_RUNTIME_DIR="$ROOT/runtime" \
  XDG_DATA_HOME="$ROOT/data" XDG_CACHE_HOME="$ROOT/cache" \
  yazi --client-id 987654323 "$ROOT/files/mixed"
```

In a control terminal, set the same `ROOT` and `STENCIL_FIXTURE_ROOT` values. Load the fixture and open/adopt the companion:

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

### Regression matrix

| Scenario | Required evidence |
| --- | --- |
| Click `c.txt`, then `j` | New Yazi snapshot hovers the clicked path, then its next sorted entry; native preview/highlight follow both moves |
| Parent and child single click | Yazi cwd becomes the target's containing directory and hovered URL is the exact clicked path |
| Directory/file double click | Directory changes cwd; file opener receives the exact path even when selection/old hover differs |
| External Yazi cursor/cd | Companion updates from snapshots without local input |
| Hidden toggle and filter | `.` changes Yazi's file list; `/` produces Yazi-filtered current rows; Enter retains and Esc clears |
| Persistent/visual selection | Count/check marks remain independent of cursor; visual `+`/`-` marks follow range; Esc commits/leaves visual mode |
| Yank | Returned paths, Yazi selected/yanked set, and clipboard target set agree |
| Trash cancel | All fixture files and authoritative selection remain unchanged |
| Trash exact-target commit | Approved paths/client remain stable while external cursor/selection changes; only approved fixture files are trashed |
| Broken target/send/reply | Actual error displayed; queued follow-on requests cleared; no false completion claim |
| `;` shell and `:` shell action | Unique Unicode marker file appears in the intended cwd; quotes/spaces survive; `--orphan` survives as a named manager option |
| Manager input errors | Unclosed quote, unknown actor, interactive option, forced/permanent removal, and cut are rejected |
| Long lists and small pane | Current selection stays visible; local list wheel scrolling is contained; footer remains docked |
| Wide/tall raster and SVG | Preview is contained within the column; natural dimensions do not allocate column width |
| Markdown/code/Mermaid limits | A marker beyond the former small caps renders under the new limit; whole oversize files show limit/error, not a prefix |
| Live preview settings | Save a lower/higher valid limit without a cursor move; existing companion updates; malformed JSON reports an error and correction recovers |
| Tern routing boundary | Plain PageUp/Down reaches the block; intercepted preset chords are recorded as upstream routing observations, not successful plugin paging |

Use disposable fixtures for shell, trash, yank state, openers, and extraction. For click/open tests, configure an isolated observable Yazi opener when an external application would make the result ambiguous; its marker must record the actual opened path. For asynchronous operations, inspect marker/task/filesystem outcomes separately from `ok` replies. For previews, create fixtures just below/above the configured thresholds and inspect both the rendered result and error text; a truncated tree string is not proof that content was truncated by the preview reader.

### Observed interactive results

On the runtime versions above, isolated real-Yazi/native-renderer smoke work exercised current-column click followed by `j`, parent/child single clicks, directory/file double clicks, hidden toggle, raw `;` shell input with a Unicode marker, and a `:` shell action with `--orphan`. Confirmation smoke separately observed cancellation without file mutation and exact-target trash dispatch.

Preview smoke rendered 62,463 bytes through the native code renderer and a 63,848-byte Markdown fixture through its final heading (`h-full-markdown-end`). Saving a 1 KiB code limit produced a `max_bytes` error; restoring 1 MiB recovered the preview without a cursor move. A configured code limit of `999999999` clamped to 4 MiB, and a 5 MiB fixture was rejected. The settings command initialized the actual sandbox store at `$ROOT/config/plugin-data/tern-yazi/kv.json`. The decoded PNG/small SVG and the large-SVG limitation are recorded in the preview section. These observations do not turn every row of the regression matrix into a passed automated test.

A 17 MiB image was rejected at the 16 MiB read limit, while the 10.3 MiB PNG decoded. A roughly 45 KiB Mermaid fixture rendered a native `.mfig-body.mmd` SVG measuring 165 by 40 pixels. These are observed preview results, distinct from the registration-only script; they do not assert that the final coordinated reload repeated all earlier click/shell regressions.

Stop the foreground Yazi and renderer before removing the exact disposable root. Retain screenshots/logs only when needed as regression evidence; do not leave QA fixtures in the repository or reuse the live user's Yazi configuration.

## 10. Contribution checklist

- Identify which side owns the change: Yazi manager state/actors, Tern host block/protocol, or window/layout/configuration. Keep one authoritative source for each behavior.
- Inspect the installed API declarations and Yazi executor before adding an action or option. Update request encoding and decoding together; preserve actor restrictions and exact-target validation.
- Match existing Luau/Lua indentation and identifiers; use English comments/documentation. Keep changes scoped to the requested behavior.
- Preserve stable native node keys, list IDs, selected-item semantics, and surface-scoped CSS. Test actual geometry after changes to list/preview containers.
- Add a concrete offline/registration or isolated interactive regression appropriate to the change. Do not treat a ready plugin registration as a rendered-UI test, and do not claim asynchronous completion from an acknowledgement.
- Update this guide and the minimal README when the public installation, commands, configuration, or outstanding feature set changes. Preserve the intentional ignored local planning/documentation paths.
- Add no dependency or persistent user configuration change without agreement. Keep one-off scripts and fixtures outside the checkout.
- Record only verification actually exercised, including runtime versions and upstream routing boundaries. Do not commit or push as an incidental development step.
