# tern-yazi

[English](#english) · [中文](#中文)

## English

A native Tern companion for Yazi: browse directories, inspect files, and navigate Yazi through a native pane or floating panel.

**Status:** experimental, version `0.2.0`. The active implementation uses Tern's built-in Luau runtime, not a Python companion process. The GitHub repository has been created; source publication is pending. Until the first push, use an existing local checkout rather than cloning the empty repository.

### Features

- Three Miller columns: parent directory, current directory, and child directory or file preview.
- Pane-bounded columns with local scrolling, native cursor highlighting, and full-path tooltips.
- Single-click to select and reveal an entry in Yazi; double-click to enter a directory or open a file through Yazi.
- Yazi's persistent multi-selection is marked with a green `✓` per path, independently of the cursor highlight; the header count remains visible when the cursor moves.
- Image/SVG, Markdown, Mermaid, and syntax-highlighted text/code preview paths. Supported decoding/rendering depends on Tern.
- Selection count and task counters from Yazi snapshots.
- Copy Path and Terminal Here actions in file previews.
- Global command and shortcut for opening the companion and switching between floating and tiled layouts.
- No Python, `uv`, or `tern-sdk` process dependency for the native plugin. No Yazi patches required.

This is not a complete replacement for Yazi's interface or key bindings. Archive listing/extraction and command input are experimental; see the limitations below.

### Requirements

The current Linux development environment uses:

| Component | Observed version | Purpose |
| --- | --- | --- |
| Tern | `0.6.3 (26bbf2e)` | Native Luau plugins, block UI, window layout, filesystem and process APIs |
| Yazi | `26.9.1 (Terra 2025-12-27)` | File-manager state and navigation |
| Ya | `26.9.1 (Terra 2025-12-27)` | `ya emit-to` command dispatch; available on `PATH` |

These are the versions used for local verification, not a minimum-version or cross-platform compatibility guarantee. Tern must support plugin styles and native list/image components. A Nerd Font is recommended for the file-type glyphs. Optional archive helpers are `unzip`, `tar`, and `7z`; archive support varies by format and installed tools.

Yazi and Tern must run on the same machine and see the same runtime inbox. Keep `tern` on `PATH` for Terminal Here.

### Install from a local checkout

Set `REPO` to the absolute path of your checkout:

```sh
REPO="/absolute/path/to/tern-yazi"
tern plugin link "$REPO"
tern plugin list
```

The plugin list should show `tern-yazi 0.2.0` as `ready`. Linking uses the checkout in place; keep it at that path.

Install the Yazi side in your configuration. This example uses Yazi's default Linux location; adapt the directory if you use a custom configuration home:

```sh
mkdir -p "$HOME/.config/yazi/plugins"
ln -s "$REPO/yazi-plugin/tern.yazi" "$HOME/.config/yazi/plugins/tern.yazi"
```

If the destination already exists, inspect it first; do not overwrite another plugin installation blindly. Add the following line to your existing `~/.config/yazi/init.lua`, without replacing other configuration:

```lua
require("tern"):setup()
```

Start or restart Yazi after changing its plugin configuration. Reload the Tern side after editing the native plugin:

```sh
tern plugin reload
```

### Use

1. Start Yazi with the `tern` plugin enabled. The companion does not launch Yazi itself.
2. A newer snapshot can automatically open the companion. You can also use **Ctrl+Alt+Y** or the command palette entry **Yazi: Toggle Companion Panel**.
3. A newly created companion floats in the top-right corner. Repeating the shortcut switches between floating and tiled layouts; it is not a persistent show/hide toggle.
4. Click an entry to select it and update the preview. Double-click a directory to enter it. Double-click a file to request Yazi's open action.
5. Parent/child-column clicks navigate to the selected entry's containing directory. External Yazi cursor and directory changes update the companion.

Basic keyboard mappings in the companion:

| Key | Action |
| --- | --- |
| `j` / `k`, arrow up/down | Move the cursor |
| `h`, arrow left, Backspace | Go to the parent directory |
| `l`, arrow right, Enter | Enter a directory or open the current file |
| Space | Toggle Yazi selection |
| `v` | Request Yazi visual mode |
| `y` | Copy the path and request Yazi yank |
| `o` | Open the file through Tern |
| `/` | Enter a substring filter; Enter keeps it, Esc clears it |
| Ctrl+R | Re-render from the available snapshot |
| `q` / Esc outside input modes | Exit the companion block |

Keyboard parity with Yazi is incomplete. Command input (`:`) splits arguments on whitespace and does not implement shell quoting; do not treat it as a general-purpose shell.

### Architecture

```text
Yazi + tern.yazi
  |  atomic JSON snapshots
  v
$XDG_RUNTIME_DIR/tern-yazi/state-<client-id>.json
  |  window.luau checks for newer state every 100 ms
  v
Tern companion block: host.luau + companion.css
  |  ya emit-to <client-id> <action> ...
  v
Yazi
```

The inbox falls back to `/tmp/tern-yazi` when `XDG_RUNTIME_DIR` is unset or empty. Yazi writes a temporary file and renames it to the JSON filename. Snapshots include the current directory, filenames, hover metadata, selection count, sorted absolute `selected_urls`, task counters, and a limited parent listing. The selected path set is what keeps a Space mark attached to a file after cursor movement.

The host selects the snapshot with the largest positive `ts`, falling back to `seq`; it does not use filesystem modification time or the focused Tern pane. The client ID is taken from the snapshot filename. DDS telemetry remains in the Yazi plugin, but native UI state ingestion uses files, not a DDS subscriber.

### Limits and safety

- Both native readers allow **256 KiB per snapshot**. Very large directories can exceed this limit. Parent snapshots contain at most 30 entries.
- Preview reads are limited to **8 MiB for images**, **32 KiB for Markdown/Mermaid**, and **16 KiB for general text/code**. Larger files are not guaranteed to preview completely. There is no dedicated PDF, audio, or video viewer.
- There is no client picker, client pinning, snapshot expiry, or Yazi-process liveness check. Stale snapshots can remain after Yazi exits. Closing the companion does not prevent the next newer snapshot from opening it again.
- Snapshot deduplication does not track every filename or metadata change. A same-count rename or metadata-only update may not refresh immediately.
- The `Synced` badge is currently a presentation label, not a verified connection-health indicator. Permission-like strings in the footer are placeholders based on file type, not actual filesystem permissions.
- The inbox contains local paths and metadata in plaintext. Use a trusted, user-private runtime directory shared by both processes. The `/tmp` fallback is not an authenticated or sandboxed channel; the plugin does not enforce ownership or private permissions. Use a conventional runtime path without spaces or shell metacharacters: the Yazi setup currently constructs its directory-creation command without shell quoting.
- Archive extraction writes into the current directory without confirmation or undo. ZIP extraction uses `unzip -o` and can overwrite existing files. Do not extract untrusted archives through the companion. Other archive formats depend on external tools and are not uniformly supported.

### Development and verification

| File | Role |
| --- | --- |
| `plugin.toml` | Manifest and native entry points |
| `host.luau` | Snapshot ingestion, block UI, previews, action dispatch |
| `window.luau` | Pane lifecycle, floating/tiled layout, command, shortcut, polling |
| `companion.css` | Column sizing, scrolling, image containment |
| `yazi-plugin/tern.yazi/main.lua` | Yazi snapshot export and DDS telemetry |
| `tern.d.luau` | Generated Tern API definitions for IDE tooling |
| `tests/smoke_luau.sh` | Plugin load/reload registration smoke check |

Refresh type definitions with `tern plugin types .`. The native plugin has no Python test suite in this checkout; the Python/DDS plans in `PLAN.md` and `docs/m0-findings.md` are historical, not a supported Python entry point.

The registration smoke script links/reloads/unlinks the plugin and writes a fixture named `state-9999.json`. It does **not** open the block, exercise Yazi, or verify rendered pixels. Its default inbox can collide with a real Yazi client, and `TERN_CONFIG_DIR` alone does not guarantee daemon/window isolation. Run it with a disposable runtime directory and a disposable Tern environment, not against your working session:

```sh
runtime=$(mktemp -d /tmp/tern-yazi-smoke-runtime.XXXXXX)
XDG_RUNTIME_DIR="$runtime" sh tests/smoke_luau.sh
# Remove only this disposable directory after inspection.
```

Local interactive verification has separately exercised a sandboxed Yazi instance and an isolated Tern renderer: three-column clicks, directory activation, a directory with over 200 entries, local scrolling, wide/tall raster and SVG previews, and compact-pane geometry. Registration success alone is not evidence that a new UI path works.

To remove the Tern-side link, run `tern plugin unlink tern-yazi`. Remove only the Yazi symlink and setup line you added if you also want to disable snapshot export.

---

## 中文

**tern-yazi 是 Yazi 的原生 Tern 伴随面板**：在平铺窗格或浮动面板内浏览目录、预览文件，并将导航操作回传给 Yazi。

**当前状态：实验性，版本 `0.2.0`。** 活跃实现运行在 Tern 内置 Luau VM 中，不需要外部 Python 伴随进程。GitHub 仓库已创建，但源码尚未推送；首次推送前请使用现有本地工作副本，不要从空仓库克隆安装。

### 功能

- 父目录、当前目录、子目录或文件预览组成的三列 Miller 布局。
- 面板内等高列、列内滚动、原生选中高亮、完整路径悬停提示。
- Yazi 的持久多选会在对应路径前显示绿色 `✓`；它与光标高亮相互独立，移动光标后顶部数量和逐行标记仍保留。
- 单击选中并同步 Yazi 悬停位置；双击进入目录或通过 Yazi 打开文件。
- 图片/SVG、Markdown、Mermaid、文本和代码预览路径；具体解码与渲染能力取决于 Tern。
- 展示 Yazi 快照提供的选择数量与任务计数。
- 文件预览中的 Copy Path 和 Terminal Here 操作。
- 全局快捷键和命令面板入口，支持浮动与平铺布局切换。
- 原生插件不依赖 Python、`uv` 或 `tern-sdk` 进程，也不需要修改 Yazi 源码。

这不是完整的 Yazi 界面或快捷键替代品。归档列表、解压和命令输入仍属于实验功能。

### 环境与安装

当前 Linux 本地验证使用 **Tern `0.6.3 (26bbf2e)`、Yazi/Ya `26.9.1 (Terra 2025-12-27)`**。这些不是最低版本或跨平台兼容性承诺。Tern 需要支持原生 Luau 插件、样式及列表/图片组件；`ya` 和 Terminal Here 使用的 `tern` 应位于 `PATH`。建议使用 Nerd Font 显示文件类型图标。归档功能按格式依赖 `unzip`、`tar` 或 `7z`。

安装命令见上方 [Install from a local checkout](#install-from-a-local-checkout)：

1. 将 `REPO` 设置为本地仓库的绝对路径，执行 `tern plugin link "$REPO"`，确认 `tern plugin list` 显示插件为 `ready`。
2. 将 `yazi-plugin/tern.yazi` 链接到 Yazi 配置的 `plugins/tern.yazi`。默认 Linux 位置为 `~/.config/yazi/plugins/tern.yazi`；自定义配置目录需要相应调整。目标已存在时先检查，不要强行覆盖。
3. 在现有 `~/.config/yazi/init.lua` 中添加 `require("tern"):setup()`，保留其他配置，并重启 Yazi。
4. 修改原生插件后执行 `tern plugin reload`。链接直接使用本地工作副本，不要移动其路径。

Yazi 和 Tern 必须运行在同一机器，并读取相同的 runtime inbox。

### 使用

启动启用了插件的 Yazi；伴随面板不会自行启动 Yazi。新快照可自动打开面板，也可以按 **Ctrl+Alt+Y** 或运行命令面板中的 **Yazi: Toggle Companion Panel**。

新建面板默认浮动在右上角，再次触发快捷键会切换浮动/平铺，而不是持续生效的显示/隐藏开关。关闭后，新快照仍可能重新打开面板。

单击更新选中状态和预览，双击目录进入、双击文件请求 Yazi 打开。父列或子列点击会转到对应条目的所在目录；Yazi 外部光标和目录变化也会同步到面板。

| 按键 | 操作 |
| --- | --- |
| `j` / `k`、上下箭头 | 移动光标 |
| `h`、左箭头、Backspace | 返回父目录 |
| `l`、右箭头、Enter | 进入目录或打开当前文件 |
| Space / `v` | 切换选择 / 请求 Yazi 可视选择模式 |
| `y` | 复制路径并请求 Yazi yank |
| `o` | 通过 Tern 打开文件 |
| `/` | 子串过滤；Enter 保留、Esc 清除 |
| Ctrl+R | 从现有快照重新渲染 |
| `q` / 非输入模式下的 Esc | 退出伴随块 |

快捷键并未完全对齐 Yazi。实验性 `:` 命令输入仅按空白拆分参数，不支持完整 shell 引号规则，不应作为通用 shell 使用。

### 数据流

Yazi 插件在目录、悬停及状态栏重绘时，将 JSON 写入临时文件后原子重命名；Tern 窗口侧每 100 ms 检查新状态，host 读取快照和本地文件生成 UI，再用 `ya emit-to` 回传操作。

默认路径为 `$XDG_RUNTIME_DIR/tern-yazi/state-<client-id>.json`，环境变量为空或未设置时退回 `/tmp/tern-yazi`。快照包含目录、文件名、悬停元数据、选择数量、排序后的绝对路径数组 `selected_urls`、任务计数和有限的父目录列表。这个路径集合使 Space 标记在移动光标后仍附着于文件。

多实例时按快照最大正 `ts` 选择客户端，没有正时间戳则使用 `seq`；不是按文件修改时间或当前聚焦窗格选择。客户端 ID 来自快照文件名。Yazi 侧保留 DDS 遥测，但原生 UI 通过文件接收状态。

### 限制与安全

- 快照读取上限为 **256 KiB**，超大目录可能无法正常导入；父目录快照最多包含 30 项。
- 图片读取上限 **8 MiB**，Markdown/Mermaid **32 KiB**，通用文本/代码 **16 KiB**，大文件不保证完整预览；没有专用 PDF、音频或视频查看器。
- 没有客户端选择/固定、快照过期或 Yazi 进程存活检查；Yazi 退出后旧快照可能仍被显示。
- 去重不覆盖所有文件名和元数据变化；数量不变的重命名或仅元数据变化可能不会立即刷新。
- `Synced` 目前是展示标签，不代表已验证的连接健康状态。底栏权限样式字符串按文件类型生成，并非真实权限。
- inbox 明文包含本地路径和元数据，应使用双方共享的可信用户私有 runtime 目录。插件不强制校验属主或私有权限，`/tmp` 回退不是认证或沙箱通道。Yazi setup 的目录创建命令尚未进行 shell 引号处理，因此 runtime 路径应避免空格和 shell 元字符。
- 解压直接写入当前目录，没有确认或撤销；ZIP 使用 `unzip -o`，可能覆盖现有文件。不要通过面板解压不可信归档。其他归档格式依赖外部工具，支持并不一致。

### 开发与验证

各文件职责见上方 [Development and verification](#development-and-verification)。执行 `tern plugin types .` 可更新 IDE 类型定义。当前工作副本没有 Python 测试套件；`PLAN.md` 与 `docs/m0-findings.md` 中的 Python/DDS 架构为历史材料，不是当前支持的 Python 运行入口。

`tests/smoke_luau.sh` 只验证插件链接、加载、重载和注册状态，不启动 Yazi、不打开 UI，也不验证像素或鼠标行为。它会写入并删除 `state-9999.json`；默认运行可能影响真实客户端，仅隔离 `TERN_CONFIG_DIR` 也不等于隔离 daemon/window。请使用上方临时 runtime 示例，并在可丢弃的 Tern 环境运行；检查后仅清理自己创建的临时目录。

本地另外使用隔离 Tern 渲染器和真实沙盒 Yazi，验证了三列点击、目录双击、200 多项列表、列内滚动、横竖图片/SVG 预览和小尺寸面板布局。插件 `ready` 不能替代新增 UI 路径的实际验证。

执行 `tern plugin unlink tern-yazi` 可移除 Tern 侧链接；如需同时停用 Yazi 状态导出，仅移除自己添加的插件链接和 setup 行。
