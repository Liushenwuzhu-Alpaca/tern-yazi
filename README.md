# tern-yazi

[中文](README.zh-CN.md)

Native Tern companion panel and file browser powered by Yazi.

![tern-yazi preview](assets/preview.png)

## Overview

`tern-yazi` connects [Yazi](https://github.com/sxyazi/yazi) to the [Tern](https://stencil.so/) terminal emulator via a zero-patch native bridge. It executes Yazi inside an invisible PTY and renders authoritative file trees, metadata, and previews using Tern's native Surface Protocol UI.

- **Native UI**: 3-column Miller browsing with independent scrolling, pure declarative UI, and zero web or ANSI terminal scraping.
- **Rich Previews**: Syntax-highlighted code, Markdown, Mermaid diagrams, raster/SVG images, PDF first-page covers, archive listings, and interactive text 3D models (VRML, X3DV, OBJ, ASCII STL/PLY, glTF with embedded buffers, USDA).
- **Full Navigation**: Upstream Yazi keybindings (`j/k`, `h/l`, `gg/G`, `/`, `?`, `f`, `v`, `y`, `x`, `d`).
- **Interactive Shell**: Native blocking (`:`) and non-blocking (`;`) shell commands with full PTY support.
- **Layout Themes**: Switch between 4 themes (`Current`, `Aurora Glass`, `Editorial Paper`, `Amber Ledger`) via Tern's command palette. Each theme is a standalone stylesheet in `themes/<id>.css`; see [DEVELOPMENT.md](DEVELOPMENT.md#native-layout-themes) to customize or add one.

## Requirements

- Linux (x86_64)
- [Tern](https://stencil.so/) >= 0.7.0
- [Yazi](https://github.com/sxyazi/yazi) & `ya` >= 26.9.1
- Rust & Cargo (to compile the launcher wrapper)
- A Nerd Font installed for file glyphs

## Installation

```sh
git clone https://github.com/Liushenwuzhu-Alpaca/tern-yazi.git
cd tern-yazi
tern plugin link "$PWD"
sh install.sh --real /usr/bin/yazi
export PATH="$HOME/.local/bin:$PATH"

mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$PWD/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"
```

Enable the companion plugin in your `~/.config/yazi/init.lua`:

```lua
require("tern"):setup()
```

### Usage

- **Inside Tern**: Run `yazi` in any shell. Tern automatically opens the companion panel in-place, parking the original shell. Press `q` to exit and restore the shell.
- **Outside Tern**: The wrapper executes your original `/usr/bin/yazi` with all arguments passed through transparently.
## Roadmap

- [ ] Native interactive dialog for Yazi recursive search (`s`/`S`).
- [ ] Document and media previews (multi-page PDF, audio/video metadata, binary 3D formats).
