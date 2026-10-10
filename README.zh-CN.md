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

在 Tern 中运行 `yazi`，原生浏览器占据启动面板原来的可视位置，不额外留下可见分屏，也不自动缩放。原来的 shell 面板仅被停放，进程不会替换或重启；Yazi 运行于隐藏 PTY。在 Tern 外，`yazi` 携原始参数执行原版程序。

每个托管浏览器绑定自己的客户端，插件重载后保持绑定。`q` 停止后端，在关闭原生面板前恢复同一个原始 shell 面板；Esc 取消局部弹窗或发送原生 Yazi escape，仅离线时关闭。正常返回保留已有兄弟分屏及缩放状态。直接通过 CLI 或面板菜单关闭时，会把同一个 shell 恢复到原标签页中仍存活的相邻面板旁；原标签页已删除则使用独立恢复标签页。Tern 公开 API 无法恢复已删除的标签页 ID 或塌缩分隔线的精确比例。关闭原始 shell 面板停止对应后端；仅关闭 GUI 窗口保留守护进程中的会话。

Floating shell panes also retain their original background pane and corner, without changing the background split tree or restarting the shell. Normal `q` and offline Esc restore that same floating presentation. Raw close uses the original background pane when it survives, otherwise another recorded tiled anchor at the same corner; if no anchor survives, the same shell is restored tiled in a dedicated recovery tab.

Tern 的**新建 Yazi 分块**、**Ctrl+Alt+Y** 及 **Yazi: Toggle Companion Panel** 在未选中托管浏览器时创建带真实 Yazi 后端的可用原生浏览器，不创建助手 shell 面板，不自动缩放。已有托管浏览器仅被聚焦，不改变布局。显式传入客户端 ID 的分块参数仍用于连接普通 Yazi，关闭此类连接面板不会停止独立启动的 Yazi。
安装器默认使用 `~/.local`，可通过 `--prefix DIR` 选择安装位置；将该位置的 `bin` 放在 `PATH` 首位。`sh install.sh --uninstall` 移除安装器管理的包装器与启动助手，保留原版 Yazi；自定义安装位置使用相同的 `--prefix`。

## 布局主题

在 Tern 命令面板的 **Yazi** 分组选择：

- **Yazi: Layout - Current**（默认）：保留原有 4:9:7 三列比例、密度与预览操作按钮
- **Yazi: Layout - Aurora Glass**：宽松圆角卡片，父级列 172 px、当前列 264 px
- **Yazi: Layout - Editorial Paper**：平面分隔面板，父级列 150 px、当前列 254 px，安静导航与阅读式预览
- **Yazi: Layout - Amber Ledger**：紧凑账本行，父级列 186 px、当前列 268 px，真实行序号与已选文件名托盘

剩余宽度用于预览。选择保存在插件自己的 Tern `kv.json` 中，键为 `layout_theme`，可用值为 `current`、`aurora`、`editorial`、`amber`；插件重载、新建伴随面板后仍保留，现有所有伴随面板同步更新，无需重启 Yazi。

面板宽度不足 120 列时，三种演示布局仅视觉隐藏父级列，当前与预览列仍并排显示；Current 在所有宽度保留原布局。演示布局隐藏三个预览操作按钮，但不改变键盘打开、复制路径与终端行为。四种主题共用原生列表、独立有界滚动、图片比例保护、Yazi 权威图标与元数据，持久多选标记仍与光标区分；不嵌入网页，不新增运行时、安装字体或修改全局外观与快捷键。

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
- 原生独立分块创建，以及显式客户端 ID 连接
## 尚未实现

- Tern 中 Ctrl+D/U/F/B 与 Shift+PageUp/Down 的组合键路由
- 创建、重命名、粘贴、搜索与标签页专用快捷键
- 客户端选择
- PDF、音频与视频预览
[开发文档](DEVELOPMENT.md)
