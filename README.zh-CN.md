# tern-yazi

[English](README.md)

基于 Yazi 的 Tern 原生伴随面板与文件浏览器。

![tern-yazi 预览](assets/preview.png)

## 产品介绍

`tern-yazi` 通过零侵入原生桥接将终端文件管理器 [Yazi](https://github.com/sxyazi/yazi) 无缝接入 [Tern](https://stencil.so/)。它在后台不可见 PTY 中驱动 Yazi，并通过 Tern 声明式 Surface Protocol UI 原生渲染权威文件树、属性元数据与富文本预览。

- **原生界面**：三列米勒（Miller Columns）浏览布局，各列独立有界滚动，纯原生声明式渲染，无网页嵌入、无 ANSI 终端刮取。
- **丰富预览**：支持语法高亮代码、Markdown、Mermaid 图表、位图/矢量 SVG 图片、PDF 首页封面、压缩包列表，以及可交互的文本 3D 模型（VRML、X3DV、OBJ、ASCII STL/PLY、内嵌缓冲区的 glTF、USDA）。
- **完整导航**：全面对齐官方 Yazi 快捷键（`j/k`、`h/l`、`gg/G`、`/`、`?`、`f`、`Space`、`v`、`y`、`x`、`d`）。
- **交互 Shell**：支持阻塞式（`:`）与非阻塞式（`;`）命令，带完整交互式 PTY 终端支持。
- **布局主题**：提供 8 款原生布局主题（`Current`、`Aurora Glass`、`Editorial Paper`、`Amber Ledger`、`Blueprint Draft`、`Brutalist Block`、`Zen Minimal`、`Swiss Grid`），在命令面板中通过唯一的 `Yazi: Layout Theme…` 选择器即时切换。每个主题是 `themes/<id>.css` 下的独立样式文件，自定义或新增主题见 [DEVELOPMENT.md](DEVELOPMENT.md#native-layout-themes)。

## 环境要求

- Linux (x86_64)
- [Tern](https://stencil.so/) >= 0.7.0
- [Yazi](https://github.com/sxyazi/yazi) 与 `ya` >= 26.9.1
- Rust 与 Cargo（用于编译安装包装器）
- 已安装 Nerd Font 字体（用于文件图标）

## 安装方式

```sh
git clone https://github.com/Liushenwuzhu-Alpaca/tern-yazi.git
cd tern-yazi
tern plugin link "$PWD"
sh install.sh --real /usr/bin/yazi
export PATH="$HOME/.local/bin:$PATH"

mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$PWD/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"
```

在 `~/.config/yazi/init.lua` 中启用插件：

```lua
require("tern"):setup()
```

### 使用方式

- **在 Tern 内**：在任意终端输入 `yazi`，将在原分块位置就地唤起伴随面板并暂存原 Shell。按 `q` 即可退出并恢复原 Shell 面板。
- **在 Tern 外**：包装器自动透明透传所有参数并直接执行原版 `/usr/bin/yazi`。

## 下一步开发计划

- [ ] 接入 Yazi 递归搜索（`s`/`S`）的原生交互弹窗。
- [ ] 扩展文档与富媒体预览支持（PDF 多页、音视频元数据展示、二进制 3D 格式）。
