# tern-yazi

[English](README.md)

由 Yazi 驱动的原生 Tern 文件浏览器。

![Yazi 原生伴随面板预览](assets/preview.png)

## 环境

- Linux · Tern 0.7.0 · Yazi + Ya 26.9.1
- `tern`、`yazi`、`ya` 位于 `PATH`；Tern 与 Yazi 同机运行，共用 `XDG_RUNTIME_DIR`
- 安装器构建使用 Rust 与 Cargo
- 文件图标使用 Nerd Font

## 安装

```sh
git clone https://github.com/Liushenwuzhu-Alpaca/tern-yazi.git
cd tern-yazi
tern plugin link "$PWD"
sh install.sh --real /usr/bin/yazi
export PATH="$HOME/.local/bin:$PATH"
mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$PWD/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"
```

在 Yazi `init.lua` 添加以下内容：

```lua
require("tern"):setup()
```

在 Tern 中运行 `yazi`，打开原生缩放浮层，保留所属 shell 与原始分屏树，Yazi 运行于隐藏 PTY。在 Tern 外，`yazi` 携原始参数执行原版程序。

每个托管浮层绑定自己的客户端，插件重载后保持绑定。`q` 关闭浮层、停止后端并返回原始 shell 与其原有缩放状态；Esc 取消局部弹窗或发送原生 Yazi escape（仅离线时关闭浮层）。关闭托管面板或启动 shell 面板也会停止后端。关闭 Tern 窗口后，托管会话保留在守护进程中。

使用 **Ctrl+Alt+Y** 或 **Yazi: Toggle Companion Panel** 聚焦托管浮层，或连接普通运行中的 Yazi 并支持浮动与停靠布局切换。关闭连接面板后，原有 Yazi 继续运行。
安装器默认使用 `~/.local`，可通过 `--prefix DIR` 选择安装位置；将该位置的 `bin` 放在 `PATH` 首位。`sh install.sh --uninstall` 移除安装器管理的包装器与启动助手，保留原版 Yazi；自定义安装位置使用相同的 `--prefix`。

## 功能

- 目录面包屑使用独立按钮，支持悬停高亮与各段完整前缀路径提示；省略号展开祖先目录，末尾空白处打开原生单行目录编辑器（`Enter` 跳转、`Esc` 取消，重复点击按光标位置编辑）
- 预留 command/find/filter 输入栏位，各列标题保持单行截断
- 对齐 Yazi 官方原生 shell 语义（参考官方 [快速入门](https://yazi-rs.github.io/docs/quick-start) 与预设 [default keymap](https://github.com/sxyazi/yazi/blob/shipped/yazi-config/preset/keymap-default.toml)）：
  - `:` 运行原生阻塞式 shell（`shell --block --interactive`）：打开交互输入栏，唤起真实可见 PTY 终端显示标准输出、标准错误与退出状态码，按 Enter 键返回浏览器
  - `;` 运行原生非阻塞 shell（`shell --interactive`）：打开交互输入栏，直接提交 Yazi 原生后台任务并即刻返回浏览器（非 orphan 分离进程，无终端弹窗）
  - 输入命令无需附加 `shell` 前缀；原始输入直接传递给 shell，所有宏与变量展开完全委托给 Yazi 原生处理
- 基于 Yazi 权威快照元数据的三列浏览（当前、父级及预览列均直接消费 `file_dirs`，完全避免为类型探测进行文件系统目录枚举；保持 256 KiB 状态上限与父级/预览至多 30 项限制）；单击预览/定位，双击进入目录或打开文件
- 键盘导航与翻页：
  - `j/k` 上下移动、`h/l` 离开/进入目录、`H/L` 历史后退/前进
  - `gg/G` 跳至首行/尾行（`g` 作为前缀引导键）、PageUp/PageDown 视口翻页
- Yazi 多选标记、可视模式与任务状态：
  - `Space` 切换选择并下移光标、`v` 可视模式、`V` 取消可视模式、`Ctrl+A`/`Ctrl+R` 全选/反选
  - `y` 原生复制、`x` 原生剪切（执行原生 yank 并由 Yazi 自动清除选中）、`d` 精确路径回收站确认
- `/` 向前、`?` 向后执行原生智能查找，只移动光标而不隐藏条目；`n/N` 跳至下一个/上一个匹配，`f` 原生实时智能过滤，`.` 切换隐藏文件
- 选中文件时通过 `o`/`Enter`、双击文件及预览卡片按钮直接由 Tern 打开文件（`cx:open`），目录进入（`l`、`Enter`、双击目录）则由 Yazi 原生管理
- `q` 在输入与确认之外关闭伴随面板；`Esc` 遵循原生 Yazi 顺序（先退出可视模式或清除查找，再清除保留的过滤），编辑时取消本地输入/回收站确认并清除对应原生查找/过滤，仅离线时关闭面板
- 图片/SVG、Markdown、Mermaid 与代码高亮预览
- 通过 **Yazi: Configure Preview Limits** 配置预览读取大小
- 普通连接支持浮动与平铺面板
## 尚未实现

- Tern 中 Ctrl+D/U/F/B 与 Shift+PageUp/Down 的组合键路由
- 创建、重命名、粘贴、搜索与标签页专用快捷键
- 客户端选择
- PDF、音频与视频预览
[开发文档](DEVELOPMENT.md)
