# tern-yazi

[中文](README.zh-CN.md)

Native Tern file browser powered by Yazi.

![Native Yazi companion preview](assets/preview.png)

## Requirements

- Linux · Tern 0.7.0 · Yazi + Ya 26.9.1
- `tern`, `yazi`, `ya` on `PATH`; Tern and Yazi share the same machine and `XDG_RUNTIME_DIR`
- Rust and Cargo for the installer build
- Nerd Font for file icons

## Install

```sh
git clone https://github.com/Liushenwuzhu-Alpaca/tern-yazi.git
cd tern-yazi
tern plugin link "$PWD"
sh install.sh --real /usr/bin/yazi
export PATH="$HOME/.local/bin:$PATH"
mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$PWD/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"
```

Add to your Yazi `init.lua`:

```lua
require("tern"):setup()
```

Run `yazi` inside Tern to show the native browser in the launching pane's original visual slot, without another visible split or automatic zoom. Tiled panes retain their leaf and dividers; floating panes retain the same background pane and corner without changing its split tree. The actual shell pane is parked, not replaced or restarted; Yazi runs in an invisible PTY. Outside Tern, `yazi` runs the original executable with your arguments.

Each managed browser stays pinned to its client across plugin reloads. `q` stops its backend and restores the same original shell pane before closing; Esc cancels local modals or dispatches native Yazi escape (closing only when offline). Existing sibling splits, floating presentation and zoom are preserved by this normal return path. Raw CLI/pane close restores the same shell in its original tab using a surviving recorded anchor: a tiled shell goes beside it, while a floating shell returns over its original background pane and corner when that pane survives. If the background pane was removed, the float uses another surviving recorded tiled anchor; if none survive, the shell returns tiled in a dedicated recovery tab. Tern's public API cannot recover a removed tab ID or exact collapsed divider ratios. Closing the original shell pane stops that client's backend. Closing only the GUI window preserves the daemon-backed session.

Tern's **New Yazi block**, **Ctrl+Alt+Y**, and **Yazi: Toggle Companion Panel** create a usable native browser with its own real Yazi backend when no managed browser is selected. This standalone entry creates no helper shell pane and does not zoom. Toggle focuses an existing managed browser without changing its layout. Explicit client-ID block arguments remain attach-only; closing such an attached block leaves its independently launched Yazi running.

The installer defaults to `~/.local`; `--prefix DIR` selects another location. Place its `bin` first on `PATH`. `sh install.sh --uninstall` removes the owned wrapper and helper while preserving the original Yazi; use the same `--prefix` for a custom installation.

## Layout themes

Choose a layout from Tern's command palette under **Yazi**:

- **Yazi: Layout - Current** (default): the original 4:9:7 three-column layout, density and preview controls
- **Yazi: Layout - Aurora Glass**: roomy rounded cards, 172 px parent and 264 px current columns
- **Yazi: Layout - Editorial Paper**: flat separator panels, 150 px parent and 254 px current columns, quiet navigation and a reading-oriented preview
- **Yazi: Layout - Amber Ledger**: compact ledger rows, 186 px parent and 268 px current columns, real row ordinals and selected-filename chips

The remaining width belongs to the preview. The plugin-local `layout_theme` setting in Tern's plugin `kv.json` persists the choice across plugin reloads and new companion panes; all existing companion panes update without restarting Yazi. Values are `current`, `aurora`, `editorial`, and `amber`.

Below 120 pane columns, the three demo layouts hide the parent visually and retain side-by-side current and preview columns. Current keeps its original layout at every width. Demo layouts omit the three preview action buttons; keyboard opening, path copying and terminal behavior remain unchanged. All themes use the same native lists, bounded independent scrollers, image containment, authoritative Yazi icons/metadata and persistent selection marks. They require no web embedding, new runtime, installed font or global appearance/keybinding changes.

## Features

- Independent directory breadcrumb buttons with hover feedback and full-prefix tooltips; an ancestor ellipsis and trailing native single-line directory editor (`Enter` to jump, `Esc` to cancel, repeated clicks to edit at the caret)
- Reserved command/find/filter input row and single-line column headings
- Native Yazi shell execution aligned with upstream [Quick Start](https://yazi-rs.github.io/docs/quick-start) and the shipped [default keymap](https://github.com/sxyazi/yazi/blob/shipped/yazi-config/preset/keymap-default.toml):
  - `:` runs a raw blocking shell (`shell --block --interactive`): opens an input prompt, runs a real visible PTY terminal with stdout, stderr, and interactive stdin, printing the exit code until Enter returns to the browser
  - `;` runs a raw non-blocking shell (`shell --interactive`): opens an input prompt and directly submits a native Yazi background task, returning to the browser promptly (not orphan, no terminal pane)
  - Commands require no `shell` prefix; raw input passes directly to the shell and delegates all macro and variable expansion to Yazi
- Three-column browsing powered by authoritative Yazi snapshot metadata (`file_dirs` for current, parent, and preview columns with zero directory enumeration for type probes; 256 KiB state cap, parent/preview bounded to max 30 entries); click to preview/reveal, double-click to enter directory or open file
- Navigation and pagination:
  - `j/k` up/down, `h/l` leave/enter directory, `H/L` history back/forward
  - `gg/G` jump to top/bottom (`g` acts as leader key for `gg`), PageUp/PageDown viewport paging
- Yazi selection marks, visual mode and task status:
  - `Space` toggles selection and advances cursor, `v` visual mode, `V` unset visual mode, `Ctrl+A`/`Ctrl+R` toggle all
  - `y` native yank (copy), `x` native yank cut (selection cleared automatically by Yazi), `d` exact-path Trash confirmation
- `/` forward and `?` backward native smart find move the cursor without hiding entries; `n/N` jump to the next/previous match; `f` applies native live smart filtering; `.` toggles hidden files
- `o`/`Enter` on a file, file double-click, and preview card button open files directly through Tern (`cx:open`), while directory entering (`l`, `Enter`, double-click) remains managed by Yazi
- `q` closes companion outside editing/confirmation; `Esc` follows native Yazi ordering (visual mode and find clear before a retained filter), cancels local input/trash and the corresponding native find/filter while editing, and closes only when offline
- Images/SVG, Markdown, Mermaid and highlighted code previews
- Configurable preview limits: **Yazi: Configure Preview Limits**
- Native standalone block creation and explicit client-ID attach

## Pending

- Ctrl+D/U/F/B and Shift+PageUp/Down routing in Tern
- Dedicated create, rename, paste, search and tab shortcuts
- Client picker
- PDF, audio and video previews
[Development guide](DEVELOPMENT.md)
