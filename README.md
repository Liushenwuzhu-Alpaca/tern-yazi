# tern-yazi

[中文](README.zh-CN.md)

Native Tern companion for Yazi.

![Native Yazi companion preview](assets/preview.png)

## Requirements

- Linux · Tern 0.7.0 · Yazi + Ya 26.9.1
- `tern`, `yazi`, `ya` on `PATH`; Tern and Yazi share the same machine and `XDG_RUNTIME_DIR`
- Nerd Font for file icons

## Install

```sh
git clone https://github.com/Liushenwuzhu-Alpaca/tern-yazi.git
cd tern-yazi
tern plugin link "$PWD"
mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$PWD/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"
```

Add to your Yazi `init.lua`, then restart Yazi:

```lua
require("tern"):setup()
```

Open with **Ctrl+Alt+Y** or **Yazi: Toggle Companion Panel**.

## Features

- Three-column browsing; click to preview, double-click to enter or open
- Yazi selection marks, visual selection and task status
- Images/SVG, Markdown, Mermaid and highlighted code previews
- Configurable preview limits: **Yazi: Configure Preview Limits**
- `j/k`, `h/l`, `g/G`, PageUp/PageDown navigation
- `/` filtering, `.` hidden files, `;` shell input, `:` manager commands
- `d` exact-path Trash confirmation, `y` yank, path copy and archive tools
- Floating and tiled panels

## Pending

- Ctrl+D/U/F/B and Shift+PageUp/Down routing in Tern
- Dedicated create, rename, paste, search and tab shortcuts
- Client picker and connection-health status
- PDF, audio and video previews

[Development guide](DEVELOPMENT.md)
