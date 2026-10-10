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

Each managed overlay stays pinned to its client across plugin reloads. `q` closes it, stops its backend, and returns to the original shell in its prior zoom state; Esc first cancels input or Trash confirmation, clears a filter, or leaves visual selection, then closes. Closing the managed pane or launching shell pane also stops the backend. Managed sessions persist in Tern's daemon when their window closes.

Use **Ctrl+Alt+Y** or **Yazi: Toggle Companion Panel** to focus the managed overlay, or to attach to an ordinary running Yazi with floating and docked panel layouts. Closing an attached panel leaves that Yazi running.

The installer defaults to `~/.local`; `--prefix DIR` selects another location. Place its `bin` first on `PATH`. `sh install.sh --uninstall` removes the owned wrapper and helper while preserving the original Yazi; use the same `--prefix` for a custom installation.

## Features

- Dedicated path card with single-line truncation and full-path tooltip, separate status badges, and single-line column headings
- `;` raw shell and `: shell quoted-run --block` launch a real visible PTY terminal with stdout, stderr, and interactive stdin, printing the exit code until Enter returns to Yazi
- Explicit `--orphan` executes native detached commands with notification feedback
- Three-column browsing; click to preview, double-click to enter or open
- Yazi selection marks, visual selection and task status
- Images/SVG, Markdown, Mermaid and highlighted code previews
- Configurable preview limits: **Yazi: Configure Preview Limits**
- `j/k`, `h/l`, `g/G`, PageUp/PageDown navigation
- `/` filtering, `.` hidden files, `:` manager commands
- `d` exact-path Trash confirmation, `y` yank, path copy and archive tools
- Floating and docked panels for ordinary attach

## Pending

- Ctrl+D/U/F/B and Shift+PageUp/Down routing in Tern
- Dedicated create, rename, paste, search and tab shortcuts
- Client picker
- PDF, audio and video previews

[Development guide](DEVELOPMENT.md)
