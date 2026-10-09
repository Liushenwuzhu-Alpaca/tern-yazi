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
| `l`, arrow right | Enter the visible directory (no file open) |
| Enter | Enter a directory or request an explicit-path Yazi open |
| Space | Toggle the visible path in Yazi; green `✓` remains authoritative |
| `v` / Esc in visual mode | Start visual selection / commit and leave visual mode; amber `+`/`-` range marks are separate from persistent selection |
| `y` | Yazi yank selected-or-visible paths and copy the same paths to the clipboard |
| `o` | Open the visible path through Tern |
| `d` | Prepare selected-or-visible targets and show an exact-path native confirmation; Enter confirms, Esc cancels without effects |
| `g` / Home, `G` / End | First / last item |
| PageDown/Up | Full Yazi viewport down / up |
| `/` | Enter a Yazi regex filter (case-insensitive); Enter retains it, Esc clears it before closing the block |
| Ctrl+R | Re-render from the available snapshot |
| `x` | Extract the visible archive, not cut |
| `q` / Esc without input, filter or visual mode | Exit the companion block |

Keyboard parity with Yazi is incomplete. `:` accepts a manager action and arguments, with single/double quotes and backslash escapes; it is not a shell (no expansion, pipes, globbing or substitution). For example, `cd '/absolute/path/Mixed Case'`. Typing and pasting preserve case, spaces and Unicode; Backspace removes one Unicode codepoint, not a grapheme cluster. Enter executes the submitted action without opening an extra Yazi shell prompt; Esc cancels locally. Invalid quoting, unknown actions, spawn/send failures and missing acknowledgements are errors, not success. An actor acknowledgement is not proof that asynchronous open, shell or trash work completed. Destructive force/permanent removal is disabled in command input; `remove` uses the same native confirmation as `d`. Plain `D` is deliberately unsupported.

On Tern 0.7.0, default scoped view bindings intercept Ctrl+D/U/F/B before a plugin block receives them; Shift+PageUp/Down also does not reach the block in the observed runtime. Plugin binds for preset-owned chords are rejected, and scoped actions cannot use the built-in command override API. The host implements half/full-page semantics when delivered, but these physical shortcuts remain unavailable without a Tern input-routing change. The plugin does not silently rewrite persistent user keybindings.

Local commands deliberately exclude other plugins, interactive flags and prompt-based create/rename/search/find/tab/bulk operations. Supported action names are `cd`, `arrow`, `leave`, `enter`, `back`, `forward`, `reveal`, `follow`, `stash`, `open`, `yank`, `unyank`, `toggle`, `toggle_all`, `visual_arrow`, `visual_mode`, `escape`, `copy`, `shell` (non-interactive), `hidden`, `linemode`, `filter`, `filter_do`, `sort`, `refresh`, `quit`, `close`, `suspend`, `seek`, and confirmed `remove`. This is a local manager-command interface, not a safe shell sandbox.

### Architecture

```text
Yazi + tern.yazi
  |  atomic JSON snapshots
  v
$XDG_RUNTIME_DIR/tern-yazi/state-<client-id>.json
  |  window.luau checks for newer state every 100 ms
  v
Tern companion block: host.luau + companion.css
  |  serialized ya emit-to <client-id> plugin tern <quoted-request-json>
  v
Yazi
```

The inbox falls back to `/tmp/tern-yazi` when `XDG_RUNTIME_DIR` is unset or empty. Yazi atomically exports current filtered/sorted filenames, hover metadata, sorted absolute `selected_urls`, mode and `marked_urls`, tasks and a limited parent listing. Each command has a unique request ID and atomic `reply-<id>.json`; Tern removes consumed replies and waits for actor acknowledgement before sending the next request. Native trash confirmation freezes the originating client and all exact paths; changing Yazi's cursor/selection afterwards cannot retarget confirmation. Cancel sends no mutation. Confirmation dispatch validates every file, constructs the exact authoritative selection in a priority FIFO sync stage, checks the resulting set, and calls Yazi's non-permanent trash actor. Filesystem work still completes asynchronously through Yazi tasks.

The host selects the snapshot with the largest positive `ts`, falling back to `seq`; it does not use filesystem modification time or the focused Tern pane. The client ID is taken from the snapshot filename. DDS telemetry remains in the Yazi plugin, but native UI state ingestion uses files, not a DDS subscriber.

### Limits and safety

- Both native readers allow **256 KiB per snapshot**. Very large directories can exceed this limit. Parent snapshots contain at most 30 entries.
- Preview reads are limited to **8 MiB for images**, **32 KiB for Markdown/Mermaid**, and **16 KiB for general text/code**. Larger files are not guaranteed to preview completely. There is no dedicated PDF, audio, or video viewer.
- There is no client picker, client pinning, snapshot expiry, or Yazi-process liveness check. Stale snapshots can remain after Yazi exits. Closing the companion does not prevent the next newer snapshot from opening it again.
- Snapshot deduplication tracks filenames, persistent selection and visual range; metadata-only updates may not refresh immediately.
- The `Synced` badge is a presentation label, not a verified connection-health indicator. Permission-like strings in the footer are placeholders based on file type, not actual filesystem permissions. Missing replies time out with an unknown-outcome error: do not blindly retry a destructive request. Late unconsumed replies can remain in the runtime inbox.
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

Refresh type definitions with `tern plugin types .`. The native plugin has no Python test suite in this checkout and does not provide a supported Python entry point.

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
| `l`、右箭头 | 进入当前可见目录；不打开普通文件 |
| Enter | 进入目录或按明确路径请求 Yazi 打开 |
| Space / `v` | 切换当前可见路径的持久选择 / 开启可视范围；绿色 `✓` 与黄色 `+`/`-` 范围、光标高亮互不混用 |
| 可视模式 Esc | 提交范围并退出可视模式 |
| `y` / `o` | Yazi yank 所选集合或可见路径并复制同一集合 / 通过 Tern 打开可见路径 |
| `d` | 冻结所选集合或可见路径，在原生面板逐项确认；Enter 移到回收站，Esc 取消且不改变选择或文件 |
| `g` / Home、`G` / End | 首项 / 末项 |
| PageDown/Up | Yazi 整个视口向下 / 向上 |
| `/` | Yazi 正则过滤，不区分大小写；Enter 保留，Esc 先清除，不关闭面板 |
| Ctrl+R / `x` | 从现有快照重绘 / 解压归档（不是剪切） |
| `q` / 无输入、过滤和可视模式时的 Esc | 退出伴随块 |

快捷键并未完全对齐 Yazi。`:` 接受 manager action 与参数，支持单/双引号和反斜杠转义，不执行 shell 展开、管道、通配符或替换，例如 `cd '/absolute/path/Mixed Case'`。输入和粘贴保留大小写、空格和 Unicode，Backspace 删除一个码点（不是整个字素）。Enter 直接提交，不额外打开 Yazi shell 输入框；Esc 仅取消本地输入。引号错误、未知动作、启动/发送失败或缺少回执会报错，绝不伪报成功。actor 接受不等于异步打开、shell 或回收站任务完成。命令中禁止 force/permanent 删除；`remove` 与 `d` 使用相同原生确认，普通 `D` 不实现。

Tern 0.7.0 的默认 scoped view 绑定在 block 之前截获 Ctrl+D/U/F/B，实测 Shift+PageUp/Down 也没有到达 block。插件注册 preset 占用按键会被拒绝，scoped action 不能使用内建 command override。host 已实现收到这些键时的半/整页语义，但实体快捷键仍需要 Tern 输入路由修复；插件不会暗中修改用户持久快捷键设置。

本地命令明确拒绝其他插件、interactive 参数和 create/rename/search/find/tab/bulk 等额外输入提示操作。支持的动作名单见英文段落；`shell` 只支持非交互提交，`remove` 必须走原生确认。这是本地 manager 命令接口，不是安全 shell 沙箱。

### 数据流

Yazi 插件在目录、悬停及状态栏重绘时原子写入 JSON；Tern 窗口每 100 ms 检查，host 使用 Yazi 的已过滤/排序列表和本地预览。请求通过 `ya emit-to ... plugin tern` 串行发送，等待对应 actor 回执，不把 DDS 发送成功当作操作完成。

默认 `$XDG_RUNTIME_DIR/tern-yazi/state-<client-id>.json`，为空时退回 `/tmp/tern-yazi`。快照导出目录、文件名、悬停、`selected_urls`、mode、`marked_urls` 和任务。每请求有唯一 ID 与原子 `reply-<id>.json`，消费后删除。删除确认冻结原客户端和完整路径，之后移动 Yazi 光标或改变选择不会改掉目标；取消不发送任何选择或文件变更。确认后逐文件验证，通过优先 FIFO 同步阶段设置并核对精确选择集合，然后调用 Yazi 非永久回收站 actor；实际文件任务仍是异步的。

多实例时按快照最大正 `ts` 选择客户端，没有正时间戳则使用 `seq`；不是按文件修改时间或当前聚焦窗格选择。客户端 ID 来自快照文件名。Yazi 侧保留 DDS 遥测，但原生 UI 通过文件接收状态。

### 限制与安全

- 快照读取上限为 **256 KiB**，超大目录可能无法正常导入；父目录快照最多包含 30 项。
- 图片读取上限 **8 MiB**，Markdown/Mermaid **32 KiB**，通用文本/代码 **16 KiB**，大文件不保证完整预览；没有专用 PDF、音频或视频查看器。
- 没有客户端选择/固定、快照过期或 Yazi 进程存活检查；Yazi 退出后旧快照可能仍被显示。
- 去重覆盖文件名、持久选择与可视范围；仅元数据变化可能不立即刷新。
- `Synced` 仍是展示标签，不代表连接健康；权限字符串不是实际权限。缺少回执会以结果未知报错，不要盲目重试破坏性操作；超时后迟到的回执可能残留在 runtime inbox。
- inbox 明文包含本地路径和元数据，应使用双方共享的可信用户私有 runtime 目录。插件不强制校验属主或私有权限，`/tmp` 回退不是认证或沙箱通道。Yazi setup 的目录创建命令尚未进行 shell 引号处理，因此 runtime 路径应避免空格和 shell 元字符。
- 解压直接写入当前目录，没有确认或撤销；ZIP 使用 `unzip -o`，可能覆盖现有文件。不要通过面板解压不可信归档。其他归档格式依赖外部工具，支持并不一致。

### 开发与验证

各文件职责见上方 [Development and verification](#development-and-verification)。执行 `tern plugin types .` 可更新 IDE 类型定义。当前工作副本没有 Python 测试套件，也不提供受支持的 Python 运行入口。

`tests/smoke_luau.sh` 只验证插件链接、加载、重载和注册状态，不启动 Yazi、不打开 UI，也不验证像素或鼠标行为。它会写入并删除 `state-9999.json`；默认运行可能影响真实客户端，仅隔离 `TERN_CONFIG_DIR` 也不等于隔离 daemon/window。请使用上方临时 runtime 示例，并在可丢弃的 Tern 环境运行；检查后仅清理自己创建的临时目录。

本地另外使用隔离 Tern 渲染器和真实沙盒 Yazi，验证了三列点击、目录双击、200 多项列表、列内滚动、横竖图片/SVG 预览和小尺寸面板布局。插件 `ready` 不能替代新增 UI 路径的实际验证。

执行 `tern plugin unlink tern-yazi` 可移除 Tern 侧链接；如需同时停用 Yazi 状态导出，仅移除自己添加的插件链接和 setup 行。
