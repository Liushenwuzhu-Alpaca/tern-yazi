# tern-yazi

[English](README.md)

基于 Yazi 的 Tern 原生文件浏览器。

![tern-yazi 预览](assets/preview.png)

## 产品介绍

`tern-yazi` 无需修改任何一方，就能把终端文件管理器 [Yazi](https://github.com/sxyazi/yazi) 接入 [Tern](https://stencil.so/)。在 Tern 的 pane 里运行 `yazi`：启动器把真正的 Yazi 放在不可见的 PTY 中，并用 Tern 原生 UI 在同一个 pane 的程序 surface 上绘制浏览器。不移动、不暂存、不拆分任何 pane；按 `q` 回到 Shell 提示符。

- **原生界面**：三列米勒（Miller Columns）布局，各列独立滚动，带路径栏、标签页和状态栏，全部是声明式 Tern UI，无网页嵌入、无 ANSI 终端刮取。
- **丰富预览**：支持语法高亮代码、Markdown、Mermaid 图表、位图/矢量 SVG 图片、PDF 首页封面、压缩包列表，以及可交互的文本 3D 模型（VRML、X3DV、OBJ、ASCII STL/PLY、内嵌缓冲区的 glTF、USDA）。
- **Yazi 快捷键**：官方按键（`j/k`、`h/l`、`gg/G`、`/`、`?`、`f`、`Space`、`v`、`y`、`x`、`p`、`d`、`a`、`r`、标签页）直接驱动真实的 Yazi。
- **Tern 操作**：`o`/Enter 在 Tern 文件块中打开文件（有选中项时打开全部选中项）；`W` 在下方打开当前目录的终端；`cc`/`cd`/`cf`/`cn` 把路径、所在目录、文件名或不带扩展名的文件名复制到剪贴板。
- **Shell**：`:` 与 `;` 把命令交给 Yazi 的 `shell` 执行（分别为阻塞与后台，与 Yazi 一致），支持 `%h`、`%s` 等 Yazi 宏；不显示命令输出。
- **布局主题**：8 款主题（`Current`、`Aurora Glass`、`Editorial Paper`、`Amber Ledger`、`Blueprint Draft`、`Brutalist Block`、`Zen Minimal`、`Swiss Grid`），在浏览器内按 `T` 选择。每个主题是 `themes/<id>.css` 下的独立样式文件，自定义或新增主题见 [DEVELOPMENT.md](DEVELOPMENT.md#layout-themes)。

## 主题

在浏览器内按 `T` 实时预览布局主题，Enter 保留所选主题。每款主题都跟随 Tern 的浅色与深色外观。

| 主题 | 浅色 | 深色 |
| --- | --- | --- |
| Current | <img src="assets/theme-current-light.png" width="380" alt="Current light"> | <img src="assets/theme-current-dark.png" width="380" alt="Current dark"> |
| Aurora Glass | <img src="assets/theme-aurora-light.png" width="380" alt="Aurora Glass light"> | <img src="assets/theme-aurora-dark.png" width="380" alt="Aurora Glass dark"> |
| Editorial Paper | <img src="assets/theme-editorial-light.png" width="380" alt="Editorial Paper light"> | <img src="assets/theme-editorial-dark.png" width="380" alt="Editorial Paper dark"> |
| Amber Ledger | <img src="assets/theme-amber-light.png" width="380" alt="Amber Ledger light"> | <img src="assets/theme-amber-dark.png" width="380" alt="Amber Ledger dark"> |
| Blueprint Draft | <img src="assets/theme-blueprint-light.png" width="380" alt="Blueprint Draft light"> | <img src="assets/theme-blueprint-dark.png" width="380" alt="Blueprint Draft dark"> |
| Brutalist Block | <img src="assets/theme-brutalist-light.png" width="380" alt="Brutalist Block light"> | <img src="assets/theme-brutalist-dark.png" width="380" alt="Brutalist Block dark"> |
| Zen Minimal | <img src="assets/theme-zen-light.png" width="380" alt="Zen Minimal light"> | <img src="assets/theme-zen-dark.png" width="380" alt="Zen Minimal dark"> |
| Swiss Grid | <img src="assets/theme-swiss-light.png" width="380" alt="Swiss Grid light"> | <img src="assets/theme-swiss-dark.png" width="380" alt="Swiss Grid dark"> |

## 环境要求

- Linux (x86_64)
- [Tern](https://stencil.so/) >= 0.7.0
- [Yazi](https://github.com/sxyazi/yazi) 与 `ya` >= 26.9.1
- Rust 与 Cargo（用于编译启动器）
- 已安装 Nerd Font 字体（用于文件图标）

## 安装方式

```sh
git clone https://github.com/Liushenwuzhu-Alpaca/tern-yazi.git
cd tern-yazi
sh install.sh --real /usr/bin/yazi
export PATH="$HOME/.local/bin:$PATH"

mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$PWD/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"

# 可选：命令面板中的 "Yazi: Open File Browser" 与 Ctrl+Alt+Y
tern plugin link "$PWD"
```

在 `~/.config/yazi/init.lua` 中启用 Yazi 插件：

```lua
require("tern"):setup()
```

### 使用方式

- **在 Tern 内**：在任意 Shell pane 中输入 `yazi`，浏览器接管该 pane，直到按 `q`。命令面板入口或 Ctrl+Alt+Y 会在右侧新开一个 pane，以当前聚焦 pane 的目录运行浏览器。
- **在 Tern 外**：包装器把所有参数原样透传给原版 Yazi。

## 下一步开发计划

- [ ] 接入 Yazi 递归搜索（`s`/`S`）的原生交互弹窗。
- [ ] 扩展文档与富媒体预览支持（PDF 多页、音视频元数据展示、二进制 3D 格式）。
