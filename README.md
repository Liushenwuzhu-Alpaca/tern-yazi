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

Run `yazi` inside Tern to open a native visual zoom overlay that preserves the owner shell and original split tree, with Yazi running in an invisible PTY. Outside Tern, `yazi` runs the original executable with your arguments.

Each managed overlay stays pinned to its client across plugin reloads. `q` closes it, stops its backend, and returns to the original shell in its prior zoom state; Esc cancels local modals or dispatches native Yazi escape (closing only when offline). Closing the managed pane or launching shell pane also stops the backend. Managed sessions persist in Tern's daemon when their window closes.

Use **Ctrl+Alt+Y** or **Yazi: Toggle Companion Panel** to focus the managed overlay, or to attach to an ordinary running Yazi with floating and docked panel layouts. Closing an attached panel leaves that Yazi running.

The installer defaults to `~/.local`; `--prefix DIR` selects another location. Place its `bin` first on `PATH`. `sh install.sh --uninstall` removes the owned wrapper and helper while preserving the original Yazi; use the same `--prefix` for a custom installation.

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
- Floating and docked panels for ordinary attach

## Pending

- Ctrl+D/U/F/B and Shift+PageUp/Down routing in Tern
- Dedicated create, rename, paste, search and tab shortcuts
- Client picker
- PDF, audio and video previews
[Development guide](DEVELOPMENT.md)
