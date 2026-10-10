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

每个托管浮层绑定自己的客户端，插件重载后保持绑定。`q` 关闭浮层、停止后端并返回原始 shell 与其原有缩放状态；Esc 先取消输入或回收站确认、清除过滤、退出可视选择，随后关闭。关闭托管面板或启动 shell 面板也会停止后端。关闭 Tern 窗口后，托管会话保留在守护进程中。

使用 **Ctrl+Alt+Y** 或 **Yazi: Toggle Companion Panel** 聚焦托管浮层，或连接普通运行中的 Yazi 并支持浮动与停靠布局切换。关闭连接面板后，原有 Yazi 继续运行。
安装器默认使用 `~/.local`，可通过 `--prefix DIR` 选择安装位置；将该位置的 `bin` 放在 `PATH` 首位。`sh install.sh --uninstall` 移除安装器管理的包装器与启动助手，保留原版 Yazi；自定义安装位置使用相同的 `--prefix`。

## 功能

- 独立路径卡片支持单行截断与完整路径提示，状态徽章独立成行，各列标题保持单行截断
- `;` 原生 shell 输入与 `: shell quoted-run --block` 打开具备标准输出、标准错误与交互式标准输入的真实可见 PTY 终端，打印退出码并在按 Enter 后返回 Yazi
- 显式 `--orphan` 原生后台分离执行并提供通知反馈
- 三列浏览，单击预览，双击进入目录或打开文件
- Yazi 多选标记、可视选择与任务状态
- 图片/SVG、Markdown、Mermaid 与代码高亮预览
- 通过 **Yazi: Configure Preview Limits** 配置预览读取大小
- `j/k`、`h/l`、`g/G`、PageUp/PageDown 键盘导航
- `/` 过滤、`.` 隐藏文件切换、`:` manager 命令
- `d` 精确路径回收站确认、`y` 复制、路径复制与归档操作
- 普通连接支持浮动与平铺面板
## 尚未实现

- Tern 中 Ctrl+D/U/F/B 与 Shift+PageUp/Down 的组合键路由
- 创建、重命名、粘贴、搜索与标签页专用快捷键
- 客户端选择
- PDF、音频与视频预览

[开发文档](DEVELOPMENT.md)
