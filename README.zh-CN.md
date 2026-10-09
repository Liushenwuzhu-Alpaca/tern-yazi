# tern-yazi

[English](README.md)

Yazi 原生 Tern 伴随面板。

![Yazi 原生伴随面板预览](assets/preview.png)

## 环境

- Linux · Tern 0.7.0 · Yazi + Ya 26.9.1
- `tern`、`yazi`、`ya` 位于 `PATH`；Tern 与 Yazi 同机运行，共用 `XDG_RUNTIME_DIR`
- 文件图标使用 Nerd Font

## 安装

```sh
git clone https://github.com/Liushenwuzhu-Alpaca/tern-yazi.git
cd tern-yazi
tern plugin link "$PWD"
mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$PWD/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"
```

在 Yazi `init.lua` 添加以下内容，随后重启 Yazi：

```lua
require("tern"):setup()
```

使用 **Ctrl+Alt+Y** 或命令面板中的 **Yazi: Toggle Companion Panel** 打开。

## 功能

- 三列浏览，单击预览，双击进入目录或打开文件
- Yazi 多选标记、可视选择与任务状态
- 图片/SVG、Markdown、Mermaid 与代码高亮预览
- 通过 **Yazi: Configure Preview Limits** 配置预览读取大小
- `j/k`、`h/l`、`g/G`、PageUp/PageDown 键盘导航
- `/` 过滤、`.` 隐藏文件切换、`;` shell 输入、`:` manager 命令
- `d` 精确路径回收站确认、`y` 复制、路径复制与归档操作
- 浮动与平铺面板

## 尚未实现

- Tern 中 Ctrl+D/U/F/B 与 Shift+PageUp/Down 的组合键路由
- 创建、重命名、粘贴、搜索与标签页专用快捷键
- 客户端选择与连接健康状态
- PDF、音频与视频预览

[开发文档](DEVELOPMENT.md)
