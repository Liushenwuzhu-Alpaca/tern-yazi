# Development Guide

This document describes the `tern-yazi` implementation: a Rust launcher that supervises the real Yazi on an invisible PTY and draws the browser on its own pane's Tern program surface, plus a Yazi Lua plugin that exports Yazi's state and executes requests. Outside Tern the installed wrapper runs the original Yazi directly. Inside Tern, `yazi` typed in a shell pane becomes that pane's foreground program; nothing is parked, split or restored, and `q` returns the shell prompt. Yazi owns navigation, selection, tasks and shell execution; the surface owns presentation, input editing, previews and confirmation. There is no Python process and no plugin block.

## 1. Environment and installation

### Runtime reference

The recorded Linux interactive verification environment uses:

| Component | Observed version | Responsibility |
| --- | --- | --- |
| Tern | `0.7.0` (`9ca00e4`) | Program surface (TSP), file blocks (`tern open`), panes (`tern split`), clipboard (OSC 52), palette |
| Yazi | `26.9.1` | Authoritative directory navigation, cursor, selection, visual marks, tasks and shell execution |
| Ya | `26.9.1` | `ya emit-to` dispatch to the supervised Yazi client |

These are observed versions, not a compatibility matrix. Keep `ya` and `tern` on `PATH`: the surface runs both as subprocesses.

Besides `std`, the launcher crate has three locked dependencies: `libc` (PTY, termios, signals, process groups), `serde_json` (inbox records and wire bodies) and `tern-sdk` 0.1.0 (the TSP session, UI tree, stylesheets and blobs). Cargo is a build-time requirement only; the installer produces a native executable. A Nerd Font supplies the glyphs selected by Yazi's theme. Archive previews use installed `unzip`, `tar` or `7z`; PDF previews use Poppler's `pdfinfo` and `pdftocairo`.

### Install the launcher and the Yazi plugin

Set `REPO` to the absolute location of a local checkout, then install the owned wrapper:

```sh
REPO=/absolute/path/to/tern-yazi
sh "$REPO/install.sh" --real /usr/bin/yazi
export PATH="$HOME/.local/bin:$PATH"
```

`--real` names the existing Yazi executable. The default prefix is `$HOME/.local`; `--prefix /absolute/prefix` changes it. Put that prefix's `bin` before the original Yazi directory on `PATH`. When `--real` is omitted, the installer searches `PATH` for an original executable, skipping its own wrapper; an owned reinstall reuses the recorded path.

The installer runs `cargo build --locked --release --manifest-path "$REPO/launcher/Cargo.toml" --target-dir "$REPO/launcher/target"`. It installs `prefix/bin/yazi`, `prefix/libexec/tern-yazi-launch`, the original-path data file `tern-yazi-launch.real`, and the SHA-256 ownership manifest `tern-yazi-launch.owned`. Existing unrelated or modified destination files cause an ownership error rather than replacement. Re-run the installation after Rust source changes; the stylesheets are compiled into the executable, so CSS changes need a rebuild too.

Install the Yazi side in your Yazi configuration and add the setup line to its `init.lua`, preserving any other configuration:

```sh
mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$REPO/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"
```

```lua
require("tern"):setup()
```

Optionally link the Tern plugin for the `Yazi: Open File Browser` palette entry and its Ctrl+Alt+Y chord (section 3):

```sh
tern plugin link "$REPO"
```

The wrapper runs `tern-yazi-launch --real ORIGINAL -- [YAZI_ARGS...]` when `TERN_PANE` is set, and the original Yazi otherwise. `--help`, `-h`, `--version` and `-V` before the argument delimiter run the original immediately. Supervised startup supplies a random client ID and rejects user `--client-id`/`--client-id=...` options. If the TSP handshake fails (not a Tern pane, or a Tern without program surfaces), the launcher runs the original Yazi with the same arguments.

Remove the launcher with `sh "$REPO/install.sh" --uninstall` (add `--prefix` for a custom prefix). Uninstall validates the owned files against the manifest, removes only those files and preserves the original Yazi and both plugin registrations.

### Editor tooling

`tern plugin types .` regenerates [`tern.d.luau`](tern.d.luau), the Tern API declarations used by `window.luau`. Yazi's `main.lua` uses Yazi's Lua API, not `tern`.

## 2. Source map

| File | Responsibility |
| --- | --- |
| [`launcher/src/main.rs`](launcher/src/main.rs) | Entry point: the `--rename-no-replace` helper mode, passthrough outside Tern, otherwise plan the launch and run the surface |
| [`launcher/src/helper.rs`](launcher/src/helper.rs) | The no-replace rename the Yazi plugin runs through the bridge's `helper` path |
| [`launcher/src/supervisor.rs`](launcher/src/supervisor.rs) | Argument parsing, launch planning, the bridge environment, spawning Yazi, the initial-snapshot handshake, bounded shutdown |
| [`launcher/src/backend.rs`](launcher/src/backend.rs) | Yazi on an invisible PTY in its own process group: output draining, terminal query answers, resize, termination |
| [`launcher/src/inbox.rs`](launcher/src/inbox.rs) | The private runtime inbox: client-ID reservation, capped no-follow reads, file stamps, teardown |
| [`launcher/src/sys.rs`](launcher/src/sys.rs) | Signals, C strings, errno and small JSON helpers |
| [`launcher/src/surface/mod.rs`](launcher/src/surface/mod.rs) | The single-threaded main loop: TSP session, snapshot reload by stamp, effects, render throttling, blob uploads, the diagnostic log |
| [`launcher/src/surface/model.rs`](launcher/src/surface/model.rs) | Snapshot parsing (state joined with its listing generation) and pure classification helpers |
| [`launcher/src/surface/actions.rs`](launcher/src/surface/actions.rs) | Serialized `ya emit-to` requests and bounded reply polling |
| [`launcher/src/surface/input.rs`](launcher/src/surface/input.rs) | Pure input reducer: keys, modes, UTF-16 editors, confirmation, theme picker; emits effects |
| [`launcher/src/surface/handover.rs`](launcher/src/surface/handover.rs) | Lent terminal: suspends the surface and relays the pane's tty to Yazi's PTY while Yazi is away from its alternate screen |
| [`launcher/src/surface/view.rs`](launcher/src/surface/view.rs) | Pure view: `(state) -> UI tree` with stable roles and keys |
| [`launcher/src/surface/format.rs`](launcher/src/surface/format.rs) | Sizes, permissions, tones, icons, breadcrumbs |
| [`launcher/src/surface/preview.rs`](launcher/src/surface/preview.rs) | Bounded file reads, archive listings, PDF covers, text 3D detection, off-thread loading |
| [`launcher/src/surface/panesize.rs`](launcher/src/surface/panesize.rs) | The pane's live rows and columns from `tern ctl state` on a helper thread |
| [`launcher/src/surface/theme.rs`](launcher/src/surface/theme.rs) | Theme registry, embedded stylesheets, persisted choice |
| [`launcher/src/surface/window.rs`](launcher/src/surface/window.rs) | Window actions: `tern open`, `tern split`, OSC 52 clipboard |
| [`companion.css`](companion.css), [`themes/<id>.css`](themes) | Base stylesheet and one stylesheet per layout theme, embedded with `include_str!` |
| [`yazi-plugin/tern.yazi/main.lua`](yazi-plugin/tern.yazi/main.lua) | Yazi plugin: atomic snapshots, request execution, exact-target trash guard |
| [`window.luau`](window.luau), [`plugin.toml`](plugin.toml) | Optional Tern plugin: the palette entry and Ctrl+Alt+Y, which open a pane running `yazi` |
| [`install.sh`](install.sh) | Owned wrapper/helper installation, original executable resolution, reversible uninstall |
| [`tests/interaction.sh`](tests/interaction.sh), [`launcher/tests/interaction.rs`](launcher/tests/interaction.rs) | Real isolated Tern/Yazi interaction suite |

## 3. Architecture and lifecycle

```text
shell pane: yazi [args]
  +-- outside Tern / no TSP --> original Yazi [args]
  +-- inside Tern --> tern-yazi-launch --real ORIGINAL -- [args]   (pane foreground program)
        |-- invisible PTY --> original Yazi --client-id CID          (own process group)
        |       \-- tern.yazi/main.lua --> state-CID.json, listing-CID.json, reply-*.json
        |-- ya emit-to CID plugin tern <request>                     (surface/actions.rs)
        |-- program surface on this pane (tern-sdk)                  (surface/*)
        \-- tern open / tern split / OSC 52                           (surface/window.rs)
```

### Startup and supervision

The launcher reserves a random positive 30-bit client ID in the runtime inbox (an exclusive `client-CID.lock`, after checking that no `state-` or `listing-` file uses the ID) and a session token. It starts Yazi on a `forkpty` PTY in its own process group with `--client-id CID`, the caller's umask, `TERN_*` pane variables removed, and `TERN_YAZI_BRIDGE` carrying `{helper}`, the path of this binary for the plugin's no-replace rename. Yazi's ANSI output stays on the invisible PTY; the backend drains it and answers basic terminal queries.

Startup waits up to 15 seconds for a valid `state-CID.json` and matching listing with the expected client ID. A missing Yazi plugin, an early Yazi exit or a timeout ends with a bounded diagnostic and exit status 1. Shutdown sends SIGTERM to the process group, allows 1.5 seconds, then SIGKILL; as a subreaper the launcher also reaps orphaned descendants. Inbox teardown removes only this client's files: state, listing, the lock and `reply-CID-TOKEN-*`.

### Main loop

The surface is a `SurfaceOptions::screen()` program surface on the launcher's own pane. The loop is single-threaded: it waits in `session.next()` for at most 25 ms, reloads the snapshot only when the state file's stamp changes, applies input effects, and renders only when something changed, at least 33 ms apart. Preview reads and window CLI calls run on helper threads and report back over channels; the loop never blocks on them. The tty belongs to the session (raw mode), so diagnostics go to the file named by `TERN_YAZI_LOG`, never to stderr. Notices (former toasts) show in the input row for about 5 seconds. When Yazi exits (`q`, or a crash), the surface closes and the shell prompt returns with Yazi's status. Closing the pane hangs up the session; the launcher then terminates Yazi's group.

Yazi's PTY follows the pane's real size. While a program surface is live, Tern freezes the pane tty's winsize and its `tern inspect` rows, and the surface `Resize` event carries only columns. The one source that still tracks the pane is `tern ctl state`, a developer command the window answers for the pane it runs in: its `panes[]` list each pane's current `cols` and `rows`. A helper thread (`surface/panesize.rs`) polls it every 500 ms (every 5 s while it fails), matches `TERN_PANE`, and the main loop resizes Yazi's PTY and the surface's row count when the pair changes. On a Tern without that command the tty size read at startup is all there is, and Yazi keeps it.

### Window actions

| Action | Keys | Mechanism |
| --- | --- | --- |
| Open in Tern | `o`, Enter on a file, double-click a file | `tern open -- PATH...`; with a selection, every selected path |
| Terminal here | `W` | `tern split $TERN_PANE down --cwd <shown directory>` |
| Copy | `cc` path, `cd` directory, `cf` name, `cn` stem | OSC 52 `ESC ] 52 ; c ; <base64> BEL` on the pane's tty; Tern sets the system clipboard. The selection, else the hovered entry, joined by newlines |

`tern open` and `tern split` act relative to `TERN_PANE`. They run off the main loop; only a failure comes back, as an error notice.

### Shell commands

`:` and `;` hand the raw text to Yazi's `shell` (`block = true` for `:`, `false` for `;`), which expands its macros (`%h`, `%s`, ...). As in Yazi, `;` is a background task whose output goes to the task log (`w`, then Enter), and `:` runs on Yazi's terminal while Yazi is away from its alternate screen.

### Lent terminal

Yazi leaves its alternate screen whenever it lends its terminal to a subprocess: `shell --block`, `ui.hide()` plugins such as `z` (fzf) and `Z` (zoxide), and task inspection. The backend watches Yazi's output for `ESC [ ? 1049 l`/`h` ([`backend.rs`](launcher/src/backend.rs) `Lent`); in between, [`surface/handover.rs`](launcher/src/surface/handover.rs) suspends the surface (TSP `suspend`), turns off the session's kitty-keys and bracketed-paste modes, writes Yazi's output to the pane and types the pane's input (minus TSP messages) into Yazi's PTY. When Yazi re-enters its alternate screen, the modes and the surface resume. The subprocess's output stays in the pane's scrollback, visible after Yazi quits, as with Yazi itself.

### Palette entry and Ctrl+Alt+Y

The optional Tern plugin is only [`window.luau`](window.luau): one palette command, `Yazi: Open File Browser` (`plugin.tern-yazi.open`), with the default chord `ctrl+alt+y`. It splits the focused pane to the right with `{cwd = <that pane's cwd>, command = "yazi"}` (a new tab when nothing is focused). The new pane's `yazi` is the installed wrapper, so the browser draws there like any other launch and the pane closes when Yazi quits.

## 4. Inbox and wire protocol

### Runtime location and trust boundary

```text
$XDG_RUNTIME_DIR/tern-yazi
```

If `XDG_RUNTIME_DIR` is unset or empty, the inbox is `/tmp/tern-yazi`. The launcher traverses the runtime path with directory descriptors and no-follow opens; an explicit runtime directory must belong to the user and have no group/other write permission. It validates the inbox owner, sets mode `0700`, and creates exclusive regular files. All reads enforce ownership, regular-file type and byte caps (256 KiB for state, 16 MiB for listings). The inbox contains local paths and selection metadata in plaintext; do not use a shared or untrusted runtime directory.

| File | Writer | Content |
| --- | --- | --- |
| `state-CID.json` | Yazi plugin | Dynamic state: cursor, hover, mode, filter, tabs, tasks, icon overrides, and the `listing_epoch`/`listing_revision` it belongs to |
| `listing-CID.json` | Yazi plugin | Current/parent/preview folders, selected and marked paths, with `epoch`/`revision` |
| `reply-<id>.json` | Yazi plugin | Per-request reply |
| `client-CID.lock` | launcher | Client-ID reservation |

Writers create `<name>.tmp` and rename it. The surface accepts a state file only together with the listing generation it names, never a mixed pair.

### Snapshot contents

The listing carries `cwd`, `files` (Yazi's filtered/sorted current listing), `file_dirs`, `file_icons`, `file_count`, bounded `parent` and `preview` folders (at most 30 entries each), `selected_urls` (sorted absolute paths) and `marked_urls` (visual marks). The state carries `client_id`, `cursor`, `hovered` (`url`, `dir`, `size`, `mtime`, `permissions`, `selected`), `mode` (`normal`, `select` or `unset`), `filter` (string, `false`, or `null` when unexposed), `tabs`, `tasks` (`total`, `succ`, `fail`, `found`, `processed`) and per-folder icon overrides.

Every real listing also has `file_icons`, keyed by the same entry names. A string is the exact icon text returned by `th.icon:match(file, { hovered = file.is_hovered })` for the active Yazi theme (including `""`); `false` records an actual nil/no-icon result. An absent map/key means unavailable metadata, not disabled icons and not a virtual archive member. Neither false, empty text, nor missing metadata receives a guessed replacement glyph. These maps stay attached to their directory snapshots, so optimistic current/parent/preview navigation resolves metadata by cwd rather than reusing the previous directory's icon map.

`file_count` on current, parent, and preview folder snapshots is the authoritative number of immediate entries in Yazi's loaded/filtered listing before the parent/preview 30-row cap. It is not a recursive descendant count or filesystem scan. An unavailable or not-yet-loaded count is omitted rather than reported as zero; a successfully loaded empty folder reports zero. `hovered.permissions` is optional authoritative Unix permission text from Yazi, not a synthesized constant. It retains its ten-character file-type prefix and special-bit `S`/`T` characters; CSS disables capitalization/small caps so ordinary `rwx` is displayed lowercase without changing its meaning.

### Requests

`surface/actions.rs` sends one request at a time:

```text
ya emit-to <client-id> plugin tern <quoted-request-json>
```

The JSON is additionally single-quoted for Yazi's plugin-argument parser, with embedded apostrophes escaped. Request IDs start with the client ID and session token so teardown can remove late replies. Mixed positional and named arguments travel as `{"positional": [...], "options": {...}}`; the plugin rebuilds the Lua table before calling the actor.

| `op` | Main fields | Plugin behavior |
| --- | --- | --- |
| `ping` | none | Forces a snapshot and acknowledges |
| `command` | `action = "shell"`, `args` | Replies `queued`, then runs Yazi's `shell` (`block` as given) |
| `action` | `action`, `args` | Runs an allowed Yazi actor (`arrow`, `leave`, `enter`, `back`, `forward`, `yank`, `paste`, `link`, `tab_*`, `visual_mode`, `escape`, `hidden`, `find_arrow`, ...) and forces a snapshot; `reveal` maps a real path to its `search://` entry; `plugin` runs only `fzf`/`zoxide`, acknowledging first |
| `search` | `query`, `via = "fd" \| "rg"` | `search_do`; Yazi streams the results into a `search://` folder |
| `tasks` | `action`, optional `step` | Acknowledges, then runs `tasks:<action>` (`show`, `close`, `arrow prev\|next`, `inspect`, `cancel`) |
| `cd` | `target` | Checks the target is a directory, then `cd` |
| `activate` | `target`, `action = "enter"` | Reveals the exact target, then `enter` |
| `toggle_advance` | none | `toggle`, then `arrow 1` |
| `filter` | `query`, `done` | `filter_do` with `smart = true` |
| `find` | `query`, `previous` | `find_do` with `smart = true` |
| `create` | `name` | Creates a file, or a directory for a trailing `/`, refusing existing targets, then reveals it |
| `rename` | `target`, `new_name` | Renames through the helper's `--rename-no-replace`, then reveals the result |
| `trash_prepare` | none | Returns the exact target paths without mutation |
| `trash_commit` | `paths` | Runs the guarded trash pipeline (section 7) |

### Replies

Replies contain `id` and `ok`, plus `error` on failure or `paths`, `queued`, `target` as the operation defines. A successful `ya` exit only proves dispatch: the surface polls the reply every 50 ms, up to 200 attempts (about 10 seconds), consumes it, and only then sends the next request. A failure clears the queue and optimistic cursor/cwd and shows the actual error. An `ok` reply acknowledges acceptance, not completion of a shell command, trash job or other task; timeout means the outcome is unknown.

## 5. UI

### Regions and lists

The surface renders `main` (path bar, input row, tabs, three columns), `dock` (status footer) and `layer` (trash confirmation, theme picker, task manager) regions. Program stylesheets style descendants of a region, not the region element, so each region is `bare root -> theme wrapper (yazi.theme.<id>[.narrow]) -> region wrapper`, and list IDs are `main.theme.region.browser.columns.{parent,current,preview}.*_list`. Rows use filenames as keys and full paths as titles; in a Yazi search, rows are the paths below the searched directory, as Yazi lists them, and the path bar adds a `search: <subject>` badge. Selecting a row in the current column reveals it in Yazi; selecting in the parent or child column makes the containing directory the provisional cwd; activating a directory enters it, activating a file opens it in Tern.

Yazi snapshots stay authoritative for listing, cursor, selection, marks and tasks. The surface keeps only local editing state, a provisional `active_file`/`optimistic_cwd` for clicks still being dispatched, the trash confirmation and the request queue. A snapshot with the same source, cwd and hover does not discard a pending click.

### Layout themes

[`surface/theme.rs`](launcher/src/surface/theme.rs) registers 8 themes (`current`, `aurora`, `editorial`, `amber`, `blueprint`, `brutalist`, `zen`, `swiss`). `T` opens the in-surface picker: `j`/`k` preview live, Enter keeps the theme in `$XDG_CONFIG_HOME/tern-yazi/settings.json` (key `theme`), Esc restores the previous theme. `YAZI_THEME` overrides the saved choice at launch.

Each theme is self-contained in `themes/<id>.css`, scoped under `[data-role^="yazi.theme.<id>"]` and the region roles; its layout knobs are the `--yazi-*` custom properties (column widths, gaps, radii, row/status heights, paddings, `--yazi-pill`). Structural differences are flags on the registry entry rather than ID checks in the view: `narrow` (append `.narrow` below 120 columns), `plain_parent` (no parent-column icons), `ordinals` (listing ordinals in rows) and `tray` (a bounded selection tray in the dock). A new theme needs its stylesheet, an `include_str!` entry in `SHEETS` and an entry in `THEMES`.

## 6. Keys

| Key | Behavior |
| --- | --- |
| `j`/`k`, Down/Up | Yazi `arrow 1`/`-1` |
| `h`/`l`, Left/Right | Yazi `leave`/`enter` |
| `H`/`L` | Yazi `back`/`forward` (history) |
| `J`/`K` | Yazi `seek 5`/`-5` (preview) |
| `gg`, Home / `G`, End | `arrow top` / `arrow bot` |
| PageDown/PageUp, Ctrl+F/Ctrl+B | `arrow 100%`/`-100%` |
| Shift+PageDown/PageUp, Ctrl+D/Ctrl+U | `arrow 50%`/`-50%` |
| Enter | Enter a hovered directory; otherwise open the selection, else the hovered file, in Tern |
| `o` | Open the selection, else the hovered entry, in Tern |
| `W` | Terminal below in the shown directory (Yazi leaves `W` unbound) |
| `cc`/`cd`/`cf`/`cn` | Copy path / directory / name / stem (selection, else hover) |
| Space | `toggle`, then `arrow 1` |
| `v`/`V` | `visual_mode` / `visual_mode --unset` |
| Ctrl+A / Ctrl+R | `toggle_all --state=on` / `toggle_all` |
| `y`/`x`, `Y`/`X` | `yank` / `yank --cut`, `unyank` |
| `p`/`P` | `paste` / `paste --force` |
| `-`/`_`, Ctrl+- | `link` / `link --relative`, `hardlink` |
| `d` | Prepare and confirm Trash (section 7) |
| `a` / `r` | Create / rename editor; Enter submits, Esc cancels |
| `f` | Smart filter editor (`filter_do`, live) |
| `/`/`?`, `n`/`N` | Smart find forward/backward, next/previous match |
| `:` | Yazi `shell --block`, on the lent terminal (section 3) |
| `;` | Yazi background shell task |
| `s` / `S`, Ctrl+S | Recursive search by name via fd / by content via ripgrep (`search_do`), cancel search |
| `z` / `Z` | Yazi's `fzf` / `zoxide` plugins, on the lent terminal |
| `w` | Yazi task manager: `j`/`k` move, Enter inspects the log on the lent terminal, `x` cancels, Esc/`w` close |
| `.` | Toggle hidden files |
| `tt`, `1`-`9`, `[`/`]`, `{`/`}` | New tab, switch to tab, relative switch, swap |
| Ctrl+C | Close the active Yazi tab |
| `T` | Theme picker (Yazi leaves `T` unbound) |
| Esc | Yazi `escape`; cancels local editing or confirmation first |
| `q` | Quit Yazi; the shell prompt returns |

Editors accept printable text, Space, paste and Unicode; editing is UTF-16 aware, Backspace removes one code point, and Home/End/arrows move the caret (Shift extends in rename). A failed submission keeps the editor open. Shell input is passed raw to Yazi, which owns macro expansion (`%h`, `%s`, ...) and quoting. Alt/Meta chords are not claimed.

## 7. Previews and exact-target safety

### Previews

Preview reads are bounded before allocation: images 16 MiB (Tern's single-blob limit), Markdown 1 MiB, other text/code 1 MiB, Mermaid 256 KiB. PDFs are never read by the launcher: it only checks for a regular, non-symlink file of at most 256 MiB, and Poppler streams it. An oversized file shows a concise `Preview unavailable` with the limit, never a prefix or a raw error. Images and SVG upload as blobs; Markdown and Mermaid render as Markdown; text 3D formats (VRML, X3DV, OBJ, ASCII STL/PLY, glTF with embedded buffers, USDA) render as a Tern model fence; files with NUL bytes show a binary-file message; other text renders as code with a language from the extension. Archives list through `unzip -Z1`, `tar -tf` or `7z l -ba -slt` (50 lines cached); PDFs show page 1 through `pdftocairo` with `pdfinfo` metadata. External helpers time out after 10 seconds. Directory previews come from the snapshot's `preview` folder, never from a filesystem scan. File and directory previews share one metadata line: kind, size, local modification time (`YYYY-MM-DD HH:MM`) and Yazi's permission text.

### Two-phase trash

1. `trash_prepare` returns target paths without any mutation. Targets start with Yazi's persistent selection, add visual marks, remove unmarked ones, and fall back to the hovered entry only when that is empty.
2. The surface freezes those paths and lists every exact path in the confirmation layer.
3. While the layer is open only confirm/cancel are accepted. Cancel sends nothing.
4. Confirm sends `trash_commit` with the frozen paths.
5. Yazi resolves every approved path through `fs.file(Url(path))` before committing.
6. A synchronous stage clears visual/selection state and toggles the approved files on. It queues the `finalize` plugin stage with `mode="sync"` behind those toggles.
7. `finalize` compares normal-mode state and the entire sorted authoritative selection to the approved paths. Any mismatch refuses the operation.
8. Only after that guard does it emit `remove` with `force=true` and no permanent-removal option. Here `force` skips Yazi's second confirmation; it does not request permanent deletion.
9. A following `settled` stage snapshots and acknowledges dispatch. Task/filesystem results still determine eventual completion.

Invariants to preserve:

- A single click moves the real Yazi cursor through `reveal`; the next keyboard move starts there.
- Current rows follow Yazi's sorting, filtering and hidden toggle, never a parallel local filter.
- Raw shell input keeps spaces, Unicode, quotes and shell syntax in both `:` and `;`.
- Transport success, actor acknowledgement and task completion stay distinct claims.

## 8. Reproducible isolated QA

### Unit tests

`cargo test --locked --manifest-path launcher/Cargo.toml --bins` covers the pure parts: argument parsing, the snapshot handshake, snapshot parsing, the input reducer, editors, preview classification and themes.

### Interaction suite

`launcher/tests/interaction.rs` needs a real GUI display (X11 or Wayland), `tern`, `ya`, `/usr/bin/yazi`, `/bin/bash`, `tar` and `wl-paste`/`wl-copy`. Each run creates a private mode-0700 root with its own Tern daemon and window, `XDG_*` directories, runtime inbox, fixture files, a `YAZI_CONFIG_HOME` linked to this checkout's Yazi plugin, and a link to this checkout's Tern plugin. It never touches user configuration. `TERN_YAZI_LOG` points at `<root>/surface.log`.

```sh
./tests/interaction.sh
# or directly
cargo test --locked --manifest-path launcher/Cargo.toml --test interaction -- --test-threads=1 --nocapture
```

| Test | Covers |
| --- | --- |
| `real_browser_interactions` | Startup in the shell pane, navigation, breadcrumbs and path editing, selection, filter/find, previews (text, Markdown, SVG, PNG, code, PDF, VRML, archives), glyphs and metadata, shortcuts, `;` background shell, trash, two clients, `q`, pane close, Yazi crash |
| `real_browser_shortcuts` | Yank/cut/paste, tabs, create/rename, links |
| `real_surface_interactions` | Theme picker and all 8 themes, layout after splits, `q` back to the same shell |
| `real_window_interactions` | Opens in Tern (`o`, Enter, multi-select, double-click), `W`, `cc`/`cf` clipboard, `:` macros and cwd through Yazi, palette entry and the Ctrl+Alt+Y action |

`tern ctl key` does not go through the window keymap (built-in chords such as Ctrl+Shift+T do not fire either), so the Ctrl+Alt+Y case checks the chord in the palette row and runs the bound action by name. Missing prerequisites fail instead of skipping. Successful runs remove their roots; failures keep `failure.txt`, the window log, `surface.log` and screenshots. `TERN_YAZI_KEEP_INTERACTION_ARTIFACTS=1` keeps a successful run; `TERN_YAZI_TEST_WINDOW_HOOK=/path/to/executable` can place the owned window (`<pid> <root> start|stop`).

Before handing off a change, run:

```sh
cargo build --locked --manifest-path launcher/Cargo.toml
cargo test --locked --manifest-path launcher/Cargo.toml --bins
cargo clippy --locked --manifest-path launcher/Cargo.toml --all-targets -- -D warnings
rustfmt --edition 2021 --check <changed .rs files>
sh -n tests/interaction.sh install.sh
```

## 9. Contribution checklist

- Identify the owner of the change: Yazi state and actors (`main.lua`), supervision (`supervisor`, `backend`, `inbox`), or the surface (reducer, view, effects). Keep one authoritative source for each behavior.
- Keep `input.rs` and `view.rs` pure; I/O belongs in `surface/mod.rs`, `actions.rs`, `preview.rs` and `window.rs`. Never block the main loop.
- Update request encoding and the plugin's decoding together; preserve actor restrictions and exact-target validation.
- Preserve stable roles, keys and list IDs; stylesheets and tests select on them.
- Add a unit test for pure logic and an interaction case for observable behavior. Do not claim asynchronous completion from an acknowledgement.
- Use English comments and documentation. Add no dependency or persistent user configuration change without agreement; keep one-off scripts and fixtures outside the checkout.
