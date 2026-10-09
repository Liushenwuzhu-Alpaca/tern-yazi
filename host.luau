-- host.luau: Host half of tern-yazi plugin.
-- Registers the "companion" block, reads Yazi state from File Inbox,
-- and renders an interactive UI tree with rich native previews (Markdown, SVG,
-- Mermaid, Image, Code highlighting), parent directory preview,
-- and robust keyboard navigation mapped to Yazi.

local ui = tern.ui

local LIST_IDS = {
	parent_list = "main.browser.columns.parent.parent_list",
	files_list = "main.browser.columns.current.files_list",
	child_list = "main.browser.columns.preview.child_list",
}

type HoverInfo = {
	url: string?,
	dir: boolean?,
	size: number?,
	mtime: number?,
	selected: number?,
}

type TaskInfo = {
	total: number,
	succ: number,
	fail: number,
	found: number?,
	processed: number?,
}

type ParentInfo = {
	cwd: string?,
	files: { string }?,
}

type YaziState = {
	seq: number,
	client_id: any,
	parent: ParentInfo?,
	cwd: string,
	files: { string },
	selected: number,
	selected_urls: { string }?,
	hovered: HoverInfo?,
	tasks: TaskInfo?,
}

-- ---------------------------------------------------------------- helpers

local function clickable(node: any, act: string): any
	node.p = node.p or {}
	node.p.actions = { click = act }
	return node
end

local function state_dir(): string
	local dir = tern.getenv("XDG_RUNTIME_DIR")
	if dir and dir ~= "" then
		return dir .. "/tern-yazi"
	end
	return "/tmp/tern-yazi"
end

local function fmt_size(bytes: number?): string
	if not bytes then
		return "-"
	end
	if bytes < 1024 then
		return string.format("%d B", bytes)
	elseif bytes < 1048576 then
		return string.format("%.1f KB", bytes / 1024)
	elseif bytes < 1073741824 then
		return string.format("%.1f MB", bytes / 1048576)
	else
		return string.format("%.1f GB", bytes / 1073741824)
	end
end

local function fmt_time(ts: number?): string
	if not ts then
		return "-"
	end
	return os.date("%Y-%m-%d %H:%M:%S", ts)
end

local function basename(path: string?): string
	if not path or path == "" then
		return "Yazi"
	end
	local name = path:match("([^/]+)/*$")
	return name or path
end

local function safe_cid(cid: any): string?
	if not cid then
		return nil
	end
	if type(cid) == "string" then
		return cid
	end
	if type(cid) == "number" then
		return string.format("%.0f", cid)
	end
	return tostring(cid)
end

local function join_path(dir: string?, name: string?): string
	if not dir or dir == "" then
		return name or ""
	end
	if not name or name == "" then
		return dir
	end
	if dir:sub(-1) == "/" then
		return dir .. name
	end
	return dir .. "/" .. name
end
local function file_ext(path: string): string
	local ext = path:match("%.([^./\\]+)$")
	return ext and ext:lower() or ""
end

local function detect_lang(ext: string): string
	local map = {
		lua = "lua",
		luau = "lua",
		py = "python",
		rs = "rust",
		js = "javascript",
		ts = "typescript",
		json = "json",
		toml = "toml",
		yaml = "yaml",
		yml = "yaml",
		md = "markdown",
		markdown = "markdown",
		sh = "bash",
		bash = "bash",
		zsh = "bash",
		c = "c",
		cpp = "cpp",
		h = "c",
		html = "html",
		css = "css",
		xml = "xml",
		sql = "sql",
		mermaid = "mermaid",
		mmd = "mermaid",
	}
	return map[ext] or "text"
end

local function mime_for(ext: string): string?
	local map = {
		png = "image/png",
		jpg = "image/jpeg",
		jpeg = "image/jpeg",
		gif = "image/gif",
		webp = "image/webp",
		svg = "image/svg+xml",
	}
	return map[ext]
end

local function is_archive(ext: string): boolean
	return ext == "zip" or ext == "tar" or ext == "gz" or ext == "tgz" or ext == "bz2" or ext == "xz" or ext == "7z" or ext == "rar" or ext == "zst"
end

-- Resolves semantic icon and tone aligned with Tern / Nerd Font file tree
local function file_icon(fname: string, is_dir: boolean): (string, string)
	if is_dir then
		if fname:sub(1, 1) == "." then
			return "", "muted"
		end
		return "", "accent"
	end
	local ext = file_ext(fname)
	if is_archive(ext) then
		return "", "warn"
	elseif ext == "lua" or ext == "luau" then
		return "", "accent"
	elseif ext == "py" then
		return "", "warn"
	elseif ext == "rs" then
		return "", "error"
	elseif ext == "js" or ext == "ts" or ext == "jsx" or ext == "tsx" then
		return "", "warn"
	elseif ext == "sh" or ext == "bash" or ext == "zsh" then
		return "", "success"
	elseif ext == "c" or ext == "cpp" or ext == "h" or ext == "hpp" then
		return "", "accent"
	elseif ext == "go" then
		return "", "accent"
	elseif ext == "md" or ext == "markdown" then
		return "", "accent"
	elseif ext == "toml" or ext == "json" or ext == "yaml" or ext == "yml" then
		return "", "muted"
	elseif ext == "png" or ext == "jpg" or ext == "jpeg" or ext == "svg" or ext == "gif" or ext == "webp" then
		return "", "accent"
	elseif ext == "mp4" or ext == "mkv" or ext == "webm" or ext == "mp3" or ext == "wav" then
		return "", "accent"
	elseif ext == "pdf" then
		return "", "error"
	elseif ext == "txt" or ext == "log" then
		return "", "muted"
	end
	return "", "muted"
end

local function read_latest_state(): YaziState?
	local sdir = state_dir()
	if not tern.fs.exists(sdir) then
		return nil
	end
	local ok_list, entries = pcall(tern.fs.list, sdir)
	if not ok_list or not entries then
		return nil
	end

	local newest_content = nil
	local newest_score = -1

	for _, fname in ipairs(entries) do
		if fname:match("^state%-.+%.json$") then
			local full_path = sdir .. "/" .. fname
			local ok_read, content = pcall(tern.fs.read, full_path, 256 * 1024)
			if ok_read and content then
				local ok_json, parsed = pcall(tern.json.decode, content)
				if ok_json and type(parsed) == "table" then
					local file_cid = fname:match("^state%-(.+)%.json$")
					if file_cid then
						parsed.client_id = file_cid
					end
					local ts = parsed.ts or 0
					local score = (ts > 0 and ts) or (parsed.seq or 0)
					if score > newest_score then
						newest_score = score
						newest_content = parsed
					end
				end
			end
		end
	end
	return newest_content
end

local function fetch_archive_contents(full_url: string, ext: string, state: any)
	if not state.archive_cache then
		state.archive_cache = {}
	end
	if state.archive_cache[full_url] ~= nil then
		return
	end
	state.archive_cache[full_url] = false
	local cmd
	if ext == "zip" then
		cmd = { "unzip", "-Z1", full_url }
	elseif ext == "tar" or ext == "gz" or ext == "tgz" or ext == "bz2" or ext == "xz" then
		cmd = { "tar", "-tf", full_url }
	elseif ext == "7z" then
		cmd = { "7z", "l", "-ba", "-slt", full_url }
	else
		cmd = { "tar", "-tf", full_url }
	end

	pcall(function()
		tern.process.run(cmd, function(res)
			if res and res.status == 0 and res.stdout then
				local lines = {}
				for line in res.stdout:gmatch("[^\r\n]+") do
					if #line > 0 then
						table.insert(lines, line)
						if #lines >= 50 then break end
					end
				end
				state.archive_cache[full_url] = lines
			else
				state.archive_cache[full_url] = {}
			end
		end)
	end)
end
-- ---------------------------------------------------------------- rich preview builder

-- Rich native preview: Markdown, SVG, Mermaid, Images, Syntax-highlighted code, directory tree
local function build_rich_preview(full_url: string, is_dir: boolean, active_name: string, state: any, cx: any): Node
	if is_dir then
		local ok, list = pcall(tern.fs.list, full_url)
		if ok and list then
			if #list == 0 then
				return ui.text({ ui.span("(empty directory)", "muted") })
			end
			local preview_items = {}
			for i = 1, math.min(18, #list) do
				table.insert(preview_items, ui.text({ ui.span(" " .. list[i], "muted") }))
			end
			if #list > 18 then
				table.insert(preview_items, ui.text({ ui.span(string.format("… and %d more entries", #list - 18), "muted") }))
			end
			return ui.col(preview_items)
		else
			return ui.text({ ui.span("Directory (unreadable)", "muted") })
		end
	end
	local ext = file_ext(full_url)
	local img_mime = mime_for(ext)

	-- 0. Compressed Archive Preview (.zip, .tar, .tar.gz, .tgz, .7z, etc.)
	if is_archive(ext) then
		fetch_archive_contents(full_url, ext, state)
		local cached = state.archive_cache and state.archive_cache[full_url]
		if type(cached) == "table" and #cached > 0 then
			local entry_nodes = {}
			for i = 1, math.min(18, #cached) do
				local item_name = cached[i]
				local is_sub_dir = (item_name:sub(-1) == "/")
				local sym, tone = file_icon(item_name, is_sub_dir)
				table.insert(entry_nodes, ui.text({
					ui.span(sym .. " ", tone),
					ui.span(item_name),
				}))
			end
			if #cached > 18 then
				table.insert(entry_nodes, ui.text({ ui.span(string.format("… and %d more files inside", #cached - 18), "muted") }))
			end
			return ui.col({
				ui.text({ ui.span(string.format("Archive Contents (%d items)", #cached), "warn") }),
				ui.rule(""),
				ui.col(entry_nodes),
			})
		elseif cached == false then
			return ui.text({ ui.span("Reading archive table of contents...", "warn") })
		else
			return ui.text({ ui.span("Archive empty or unreadable", "muted") })
		end
	end

	-- 1. SVG and Image Native Preview (via cx:blob + ui.node("image", ...))
	if img_mime then
		local ok_bytes, bytes = pcall(tern.fs.read, full_url, 8 * 1024 * 1024)
		if ok_bytes and bytes and #bytes > 0 then
			local ok_blob, blob_id = pcall(function()
				return cx:blob(bytes, img_mime)
			end)
			if ok_blob and blob_id then
				return ui.node("col", { role = "yazi.image-preview", gap = "sm" }, {
					ui.node("image", {
						blob = blob_id,
						alt = active_name,
						role = "yazi.image",
						max = { w = 1, h = tostring(math.max(1, (cx.rows or 24) - 12)) .. "lines" },
					}),
					ui.text({ ui.span(string.format("Image (%s, %s)", ext:upper(), fmt_size(#bytes)), "muted") }),
				})
			end
		end
	end

	-- 2. Mermaid diagram preview (.mermaid / .mmd)
	if ext == "mermaid" or ext == "mmd" then
		local ok_read, content = pcall(tern.fs.read, full_url, 32 * 1024)
		if ok_read and content then
			local md_wrapper = "```mermaid\n" .. content .. "\n```"
			return ui.md(md_wrapper)
		end
	end

	-- 3. Markdown rich formatted document preview (.md / .markdown)
	if ext == "md" or ext == "markdown" then
		local ok_read, content = pcall(tern.fs.read, full_url, 32 * 1024)
		if ok_read and content then
			return ui.md(content)
		end
	end

	-- 4. Code / Text syntax highlighting preview
	local ok_read, content = pcall(tern.fs.read, full_url, 16 * 1024)
	if ok_read and content then
		if content:find("\0") then
			return ui.col({
				ui.text({ ui.span("Binary file (preview suppressed)", "muted") }),
				ui.text({ ui.span("Size: " .. fmt_size(#content), "strong") }),
			})
		end
		local lang = detect_lang(ext)
		return ui.code(content, lang)
	else
		return ui.text({ ui.span("File unavailable or empty", "muted") })
	end
end

-- Keep native selection reveal enabled; CSS fits the scroller to the column's
-- remaining height. List rows are taller than terminal cells, so never pad them.
local function viewport_list(items: { any }, max_lines: number, key: string, selected: number?): Node
	local node = ui.list(items)
	node.p = node.p or {}
	node.p.max = { lines = max_lines }
	node.p.key = key
	for i, row in ipairs(node.c or {}) do
		row.p.key = items[i].name
		row.p.title = items[i].url
	end
	if selected and items[selected] then
		node.p.selected = LIST_IDS[key] .. "." .. items[selected].name
	end
	return node
end

-- Native list events carry an item node id, not a numeric row index.
local function list_event_name(ev: any): string?
	if type(ev.id) ~= "string" or type(ev.item) ~= "string" then
		return nil
	end
	local prefix = ev.id .. "."
	if ev.item:sub(1, #prefix) == prefix then
		return ev.item:sub(#prefix + 1)
	end
	return nil
end

-- Optimistic input lasts until Yazi publishes a different cursor or directory.
-- Task-only pulses must not clear a click that is still being dispatched.
local function sync_hover(state: any, yz: YaziState?)
	if not yz then
		return
	end
	local source = safe_cid(yz.client_id)
	local hovered = yz.hovered and yz.hovered.url
	if state.source_client ~= source or state.source_cwd ~= yz.cwd or state.source_hover ~= hovered then
		state.active_file = nil
		state.optimistic_cwd = nil
		state.source_client = source
		state.source_cwd = yz.cwd
		state.source_hover = hovered
	end
end

-- Parent directory entries (from Yazi state, falling back to a live listing).
local function compute_parent(yz: YaziState?, cur_cwd: string): ({ string }?, string?)
	local p = yz and yz.cwd == cur_cwd and yz.parent or nil
	local parent_files = p and p.files
	local parent_cwd = p and p.cwd
	if (not parent_files or #parent_files == 0) and cur_cwd and cur_cwd ~= "/" then
		local parent_dir = cur_cwd:match("(.+)/[^/]+$") or "/"
		local ok_list, list = pcall(tern.fs.list, parent_dir)
		if ok_list and list then
			parent_files = list
			parent_cwd = parent_dir
		end
	end
	if not parent_files or #parent_files == 0 then
		return nil, nil
	end
	return parent_files, parent_cwd
end

-- Current directory entries (optimistic cwd override + filter query applied).
local function compute_file_list(state: any, yz: YaziState, cur_cwd: string): { string }
	local file_list = yz.files or {}
	if state.optimistic_cwd then
		local ok_fs, flist = pcall(tern.fs.list, cur_cwd)
		if ok_fs and flist then
			file_list = flist
		end
	end
	if state.filter_query and state.filter_query ~= "" then
		local q = state.filter_query:lower()
		local matched = {}
		for _, fname in ipairs(file_list) do
			if fname:lower():find(q, 1, true) then
				table.insert(matched, fname)
			end
		end
		file_list = matched
	end
	return file_list
end

-- Persistent Yazi selection is distinct from the native cursor highlight.
local function selection_paths(yz: YaziState): { [string]: boolean }
	local paths = {}
	for _, path in ipairs(yz.selected_urls or {}) do
		paths[path] = true
	end
	return paths
end

local function selection_mark(paths: { [string]: boolean }, path: string): any
	return ui.span(paths[path] and "✓ " or "  ", paths[path] and "success" or "muted")
end

-- ---------------------------------------------------------------- Miller columns builders

-- Left Column: Parent directory as a scrollable list; the current
-- directory row is the list selection (native active capsule).
local function build_parent_column(yz: YaziState?, cur_cwd: string, max_lines: number, selected_paths: { [string]: boolean }): Node
	local parent_files, parent_cwd = compute_parent(yz, cur_cwd)
	if not parent_files then
		return ui.card(
			{ ui.span("Parent", "muted") },
			{ ui.text({ ui.span("(root)", "muted") }) }
		)
	end

	local cur_dirname = basename(cur_cwd)
	local items = {}
	local selected = nil
	for _, fname in ipairs(parent_files) do
		local is_cur = (fname == cur_dirname)
		if is_cur then
			selected = #items + 1
		end
		local is_dir = is_cur
		if not is_dir and parent_cwd then
			local ok_dir = pcall(tern.fs.list, join_path(parent_cwd, fname))
			is_dir = ok_dir
		end
		local sym, tone = file_icon(fname, is_dir)
		table.insert(items, {
			name = fname,
			url = join_path(parent_cwd, fname),
			label = { selection_mark(selected_paths, join_path(parent_cwd, fname)), ui.span(sym .. " ", tone), ui.span(fname, is_cur and "strong" or nil) },
		})
	end

	local parent_title = (parent_cwd and basename(parent_cwd) ~= "") and basename(parent_cwd) or "/"
	return ui.card(
		{ ui.span(parent_title, "muted") },
		{ viewport_list(items, max_lines, "parent_list", selected) }
	)
end

-- Right Column child preview: Expand subfolder contents as a scrollable list
local function render_child_directory_preview(full_url: string, dir_name: string, max_lines: number, selected_paths: { [string]: boolean }): Node
	local ok, list = pcall(tern.fs.list, full_url)
	if not ok or not list then
		return ui.card(
			{ ui.span("[" .. dir_name .. "]", "accent") },
			{ ui.text({ ui.span("(unreadable directory)", "muted") }) }
		)
	end
	if #list == 0 then
		return ui.card(
			{ ui.span("[" .. dir_name .. "]", "accent") },
			{ ui.text({ ui.span("(empty directory)", "muted") }) }
		)
	end
	local child_items = {}
	for _, cname in ipairs(list) do
		local is_sub_dir = false
		local sub_path = join_path(full_url, cname)
		if tern.fs.exists(sub_path) then
			local ok_sub = pcall(tern.fs.list, sub_path)
			is_sub_dir = ok_sub
		end
		local sym, tone = file_icon(cname, is_sub_dir)
		table.insert(child_items, {
			name = cname,
			url = sub_path,
			label = { selection_mark(selected_paths, sub_path), ui.span(sym .. " ", tone), cname },
		})
	end
	return ui.card(
		{ ui.span("[" .. dir_name .. "]", "accent") },
		{ viewport_list(child_items, max_lines, "child_list", 1) }
	)
end

-- ---------------------------------------------------------------- block definition

tern.block.define("companion", {
	init = function(cx, args, saved)
		return {
			active_file = nil,
			optimistic_cwd = nil,
			filter_mode = false,
			filter_query = "",
			cmd_mode = false,
			cmd_query = "",
			archive_cache = {},
		}
	end,

	view = function(state, cx)
		local yz = read_latest_state()
		if not yz then
			return {
				main = ui.card(
					{ ui.span("Waiting for Yazi...", "muted") },
					{
						ui.text("No active Yazi instance detected."),
						ui.text({
							ui.span("Launch "),
							ui.span("yazi", "strong"),
							ui.span(" in any terminal with tern.yazi plugin enabled."),
						}),
					}
				),
				dock = ui.row({
					clickable(ui.el("button", {}, { ui.text("Check Again") }), "poll"),
				}),
			}
		end
		sync_hover(state, yz)

		local cur_cwd = state.optimistic_cwd or yz.cwd or "/"
		if state.optimistic_cwd and yz.cwd == state.optimistic_cwd then
			state.optimistic_cwd = nil
		end

		-- 1. Top Header: Minimal clean path (collapsing $HOME to ~)
		local home_dir = tern.getenv("HOME") or ""
		local display_path = cur_cwd
		if home_dir ~= "" and display_path:sub(1, #home_dir) == home_dir then
			display_path = "~" .. display_path:sub(#home_dir + 1)
		end

		local top_rows = {}
		local path_header = ui.row({
			ui.text({ ui.span(display_path, "accent") }),
			ui.badge(string.format("%d selected", yz.selected or 0), (yz.selected or 0) > 0 and "accent" or "muted"),
			ui.badge("Synced", "success"),
		})
		table.insert(top_rows, path_header)

		-- 1.1 Filter Bar (when filter active)
		if state.filter_mode or (state.filter_query and state.filter_query ~= "") then
			local f_bar = ui.row({
				ui.badge("Filter [/]", "accent"),
				ui.text({
					ui.span(" " .. (state.filter_query or ""), "strong"),
					ui.span(state.filter_mode and " ▎" or "", "accent"),
				}),
				clickable(ui.badge("Clear [Esc]", "muted"), "clear_filter"),
			})
			table.insert(top_rows, f_bar)
		end

		-- 1.2 Command Bar (when command active)
		if state.cmd_mode or (state.cmd_query and state.cmd_query ~= "") then
			local c_bar = ui.row({
				ui.badge("Command [:]", "warn"),
				ui.text({
					ui.span(" :" .. (state.cmd_query or ""), "strong"),
					ui.span(state.cmd_mode and " ▎" or "", "warn"),
				}),
				clickable(ui.badge("Cancel [Esc]", "muted"), "clear_cmd"),
			})
			table.insert(top_rows, c_bar)
		end

		-- 2. Task progress (if background tasks are running)
		if yz.tasks and yz.tasks.total and yz.tasks.total > 0 then
			local t = yz.tasks
			local done = (t.succ or 0) + (t.fail or 0)
			local running = t.total - done
			if running > 0 then
				local ratio = t.total > 0 and (done / t.total) or 0
				local task_card = ui.card(
					{ ui.span(string.format("Tasks: %d active / %d total", running, t.total), "warn") },
					{ ui.progress(ratio, string.format("%d of %d done", done, t.total)) }
				)
				table.insert(top_rows, task_card)
			end
		end

		-- 3. File List (scrollable viewport; the list widget owns scrolling)
		local file_list = compute_file_list(state, yz, cur_cwd)
		local selected_paths = selection_paths(yz)

		local hovered_url = yz.cwd == cur_cwd and yz.hovered and yz.hovered.url
		local hovered_name = hovered_url and basename(hovered_url) or nil
		local active_name = state.active_file or hovered_name or file_list[1]

		local cur_idx = 1
		for idx, fname in ipairs(file_list) do
			if fname == active_name then
				cur_idx = idx
				break
			end
		end

		-- This is only the native list ceiling, not a terminal-to-list row conversion.
		-- The columns and their scrollers take the actual remaining pane height.
		local col_lines = math.max(1, cx.rows or 24)

		-- Determine active item type & paths
		local active_full_url = active_name and join_path(cur_cwd, active_name) or cur_cwd
		local is_active_dir = false
		if active_name then
			local is_exact_hover = (hovered_name == active_name) and yz.hovered
			if is_exact_hover and yz.hovered.dir ~= nil then
				is_active_dir = yz.hovered.dir
			elseif tern.fs.exists(active_full_url) then
				local ok_dir = pcall(tern.fs.list, active_full_url)
				is_active_dir = ok_dir
			end
		end

		-- ---------------- Left Column (Parent Directory)
		local left_col_card = build_parent_column(yz, cur_cwd, col_lines, selected_paths)

		-- ---------------- Middle Column (Current Directory, scrollable)
		local middle_items = {}
		for _, fname in ipairs(file_list) do
			local is_active = (fname == active_name)
			local is_dir = false
			local item_path = join_path(cur_cwd, fname)
			if tern.fs.exists(item_path) then
				local ok_dir = pcall(tern.fs.list, item_path)
				is_dir = ok_dir
			end
			local sym, tone = file_icon(fname, is_dir)
			table.insert(middle_items, {
				name = fname,
				url = item_path,
				label = { selection_mark(selected_paths, item_path), ui.span(sym .. " ", tone), fname },
				tone = is_active and "accent" or nil,
			})
		end

		local middle_title = string.format("%s (%d/%d)", basename(cur_cwd), cur_idx, #file_list)
		local middle_col_card = ui.card(
			{ ui.span(middle_title, "strong") },
			{ viewport_list(middle_items, col_lines, "files_list", cur_idx) }
		)

		-- ---------------- Right Column (Preview / Child Subfolder Expansion)
		local right_col_card
		if not active_name then
			right_col_card = ui.card(
				{ ui.span("Preview", "muted") },
				{ ui.text({ ui.span("(no file selected)", "muted") }) }
			)
		elseif is_active_dir then
			-- Child Directory Content Expansion (exactly like Yazi right pane!)
			right_col_card = render_child_directory_preview(active_full_url, active_name, col_lines, selected_paths)
		else
			-- Rich File Preview (Code, Markdown, Image, Archive table of contents)
			local preview_content = build_rich_preview(active_full_url, false, active_name, state, cx)
			local act_buttons = {
				clickable(ui.el("button", {}, { ui.text("Open in Tern (o)") }), "open_file=" .. active_full_url),
			}
			if is_archive(file_ext(active_name)) then
				table.insert(act_buttons, clickable(ui.el("button", {}, { ui.text("Extract Here (x)") }), "extract_archive=" .. active_full_url))
			end
			table.insert(act_buttons, clickable(ui.el("button", {}, { ui.text("Copy Path (y)") }), "copy_path=" .. active_full_url))
			table.insert(act_buttons, clickable(ui.el("button", {}, { ui.text("Terminal Here") }), "terminal_here=" .. cur_cwd))

			right_col_card = ui.card(
				{ ui.span(active_name, "strong") },
				{
					ui.node("col", { role = "yazi.preview", gap = "none" }, { preview_content }),
					ui.node("row", { role = "yazi.preview-actions", gap = "xs", wrap = true }, act_buttons),
				}
			)
		end

		-- Content must not participate in column width allocation.
		left_col_card.p.role = "yazi.column.parent"
		left_col_card.p.key = "parent"
		middle_col_card.p.role = "yazi.column.current"
		middle_col_card.p.key = "current"
		right_col_card.p.role = "yazi.column.preview"
		right_col_card.p.key = "preview"
		local miller_columns = ui.node("row", { role = "yazi.columns", key = "columns" }, {
			left_col_card,
			middle_col_card,
			right_col_card,
		})
		table.insert(top_rows, miller_columns)

		-- ---------------- Bottom Status Bar (Yazi-style Pill Status Bar)
		local mode_str = state.cmd_mode and "CMD" or (state.filter_mode and "FILTER" or "NOR")
		local mode_tone = state.cmd_mode and "warn" or (state.filter_mode and "accent" or "accent")

		local perm_str = is_active_dir and "drwxrwxrwx" or "-rw-r--r--"
		local pct_num = (#file_list > 0) and math.floor((cur_idx / #file_list) * 100) or 0
		local pct_str = string.format("%d%%", pct_num)
		local pos_str = string.format("%d/%d", cur_idx, math.max(1, #file_list))

		local status_bar = ui.node("row", { role = "yazi.status", gap = "xs" }, {
			ui.badge(mode_str, mode_tone),
			ui.badge(string.format("%d sel", yz.selected or 0), (yz.selected or 0) > 0 and "warn" or "muted"),
			ui.node("text", { role = "yazi.status-name", spans = { ui.span(active_name or "", "strong") } }),
			ui.text({ ui.span(perm_str, is_active_dir and "accent" or "muted") }),
			ui.text({ ui.span(" " .. pct_str .. " ", "muted") }),
			ui.badge(pos_str, "accent"),
		})

		return {
			main = ui.col({ ui.node("col", { role = "yazi.browser", key = "browser", gap = "sm" }, top_rows) }),
			-- Region roots are stripped; preserve the row as a child of the dock.
			dock = ui.col({ status_bar }),
		}
	end,

	title = function(state)
		local yz = read_latest_state()
		if yz and yz.cwd then
			return "Companion (" .. basename(yz.cwd) .. ")"
		end
		return "Yazi Companion"
	end,

	event = function(state, ev, cx)
		local act = ev.act
		local val = ev.value
		local yz = read_latest_state()
		sync_hover(state, yz)
		local cid = safe_cid(yz and yz.client_id)
		local cwd = yz and yz.cwd or ""

		if ev.ev == "select" or ev.ev == "activate" then
			local name = list_event_name(ev)
			local cur_cwd_ev = state.optimistic_cwd or cwd
			local base, names
			if ev.id == LIST_IDS.parent_list then
				names, base = compute_parent(yz, cur_cwd_ev)
			elseif ev.id == LIST_IDS.files_list and yz then
				base = cur_cwd_ev
				names = compute_file_list(state, yz, cur_cwd_ev)
			elseif ev.id == LIST_IDS.child_list then
				local active = state.active_file
					or (yz and yz.hovered and yz.hovered.url and basename(yz.hovered.url))
				if active then
					base = join_path(cur_cwd_ev, active)
					local ok, entries = pcall(tern.fs.list, base)
					names = ok and entries or nil
				end
			end
			if not name or not base or not names or not table.find(names, name) then
				return
			end

			local target = join_path(base, name)
			local is_dir = pcall(tern.fs.list, target)
			if ev.ev == "activate" and is_dir then
				state.optimistic_cwd = target
				state.active_file = nil
				if cid then
					tern.process.run({ "ya", "emit-to", cid, "cd", target }, function() end)
				end
				cx:toast("info", "Navigating: " .. name, target)
			else
				if base ~= cwd then
					state.optimistic_cwd = base
				end
				state.active_file = name
				if cid then
					tern.process.run({ "ya", "emit-to", cid, "reveal", target }, function(result)
						if ev.ev == "activate" and result and result.status == 0 then
							tern.process.run({ "ya", "emit-to", cid, "open" }, function() end)
						end
					end)
				end
				cx:toast("info", "Selected: " .. name, target)
			end
			return
		end

		if act == "copy_path" and val then
			cx:copy(val)
			cx:toast("success", "Path copied to clipboard", val)
		elseif act == "reveal" and val then
			state.active_file = val
			if cid then
				local target = join_path(cwd, val)
				pcall(function()
					tern.process.run({ "ya", "emit-to", cid, "reveal", target }, function() end)
				end)
			end
			cx:toast("info", "Selected: " .. val, nil)
		elseif act == "enter_dir" and val then
			state.optimistic_cwd = val
			state.active_file = nil
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", cid, "cd", val }, function() end)
				end)
			end
			cx:toast("info", "Navigating: " .. basename(val), nil)
		elseif act == "open_file" and val then
			cx:open(val)
			cx:toast("info", "Opening file in editor", val)
		elseif act == "terminal_here" and val then
			cx:toast("info", "Opening terminal in " .. val, nil)
			pcall(function()
				tern.process.run({ "tern", "split", "down", "--cwd", val }, function() end)
			end)
		elseif act == "leave" then
			local cur_cwd = state.optimistic_cwd or (yz and yz.cwd) or "/"
			local parent_dir = cur_cwd:match("(.+)/[^/]+$") or "/"
			state.optimistic_cwd = parent_dir
			state.active_file = basename(cur_cwd)
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", cid, "leave" }, function() end)
				end)
			end
		elseif act == "clear_filter" then
			state.filter_mode = false
			state.filter_query = ""
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", cid, "filter" }, function() end)
				end)
			end
		elseif act == "clear_cmd" then
			state.cmd_mode = false
			state.cmd_query = ""
		elseif act == "extract_archive" and val then
			local cur_cwd = state.optimistic_cwd or (yz and yz.cwd) or "."
			local ext = file_ext(val)
			local cmd
			if ext == "zip" then
				cmd = { "unzip", "-o", val, "-d", cur_cwd }
			else
				cmd = { "tar", "-xf", val, "-C", cur_cwd }
			end
			pcall(function()
				tern.process.run(cmd, function(res)
					if res and res.status == 0 then
						cx:toast("success", "Extracted archive successfully", basename(val))
					else
						cx:toast("error", "Failed to extract archive", basename(val))
					end
				end)
			end)
			-- Handled by view re-run
		end
	end,

	-- Full Yazi keyboard navigation mapping with robust key name matching
	key = function(state, key, cx)
		local raw_k = (key.name or ""):lower()
		local yz = read_latest_state()
		sync_hover(state, yz)
		local cid = safe_cid(yz and yz.client_id)
		local cur_cwd = state.optimistic_cwd or (yz and yz.cwd) or "/"
		local files = yz and compute_file_list(state, yz, cur_cwd) or {}

		local current_name = state.active_file or (yz and yz.hovered and basename(yz.hovered.url)) or files[1]
		local cur_idx = 1
		for idx, fname in ipairs(files) do
			if fname == current_name then
				cur_idx = idx
				break
			end
		end
		-- Normalized key conditions
		local is_colon = (raw_k == ":" or raw_k == "colon" or (raw_k == ";" and key.shift))

		-- Command mode active: redirect single keystrokes to command query
		if state.cmd_mode then
			if raw_k == "escape" then
				state.cmd_mode = false
				state.cmd_query = ""
				return true
			elseif raw_k == "enter" or raw_k == "return" then
				local q = state.cmd_query or ""
				state.cmd_mode = false
				state.cmd_query = ""
				if #q > 0 and cid then
					local parts = {}
					for p in q:gmatch("%S+") do
						table.insert(parts, p)
					end
					if #parts > 0 then
						local cmd_args = { "ya", "emit-to", cid }
						for _, p in ipairs(parts) do
							table.insert(cmd_args, p)
						end
						pcall(function()
							tern.process.run(cmd_args, function() end)
						end)
						cx:toast("success", "Sent command to Yazi", q)
					end
				end
				return true
			elseif raw_k == "backspace" then
				if #(state.cmd_query or "") > 0 then
					state.cmd_query = state.cmd_query:sub(1, -2)
				else
					state.cmd_mode = false
				end
				return true
			elseif #raw_k == 1 then
				state.cmd_query = (state.cmd_query or "") .. raw_k
				return true
			end
			return true
		end

		-- Filter mode active: redirect single keystrokes to search query
		if state.filter_mode then
			if raw_k == "escape" then
				state.filter_mode = false
				state.filter_query = ""
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "filter" }, function() end)
					end)
				end
				return true
			elseif raw_k == "enter" or raw_k == "return" then
				state.filter_mode = false
				return true
			elseif raw_k == "backspace" then
				if #(state.filter_query or "") > 0 then
					state.filter_query = state.filter_query:sub(1, -2)
					if cid then
						pcall(function()
							tern.process.run({ "ya", "emit-to", cid, "filter", state.filter_query }, function() end)
						end)
					end
				else
					state.filter_mode = false
					if cid then
						pcall(function()
							tern.process.run({ "ya", "emit-to", cid, "filter" }, function() end)
						end)
					end
				end
				return true
			elseif #raw_k == 1 then
				state.filter_query = (state.filter_query or "") .. raw_k
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "filter", state.filter_query }, function() end)
					end)
				end
				return true
			end
			return true
		end

		if raw_k == "/" then
			state.filter_mode = true
			state.filter_query = ""
			return true
		elseif is_colon then
			state.cmd_mode = true
			state.cmd_query = ""
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", cid, "shell", "--interactive" }, function() end)
				end)
			end
			cx:toast("info", "Command mode active (type & Enter)", nil)
			return true
		elseif raw_k == "x" then
			if current_name and is_archive(file_ext(current_name)) then
				local full_path = join_path(cur_cwd, current_name)
				local cmd
				if file_ext(current_name) == "zip" then
					cmd = { "unzip", "-o", full_path, "-d", cur_cwd }
				else
					cmd = { "tar", "-xf", full_path, "-C", cur_cwd }
				end
				pcall(function()
					tern.process.run(cmd, function(res)
						if res and res.status == 0 then
							cx:toast("success", "Extracted archive", current_name)
						else
							cx:toast("error", "Extraction failed", current_name)
						end
					end)
				end)
				return true
			end
		end

		local is_down = (raw_k == "j" or raw_k == "down" or raw_k == "arrowdown" or raw_k == "arrow_down" or raw_k:find("down") ~= nil)
		local is_up = (raw_k == "k" or raw_k == "up" or raw_k == "arrowup" or raw_k == "arrow_up" or (raw_k:find("up") ~= nil and not raw_k:find("page")))
		local is_left = (raw_k == "h" or raw_k == "left" or raw_k == "arrowleft" or raw_k == "arrow_left" or raw_k == "backspace")
		local is_right = (raw_k == "l" or raw_k == "right" or raw_k == "arrowright" or raw_k == "arrow_right")
		local is_enter = (raw_k == "enter" or raw_k == "return")
		local is_space = (raw_k == "space" or raw_k == " ")
		local is_quit = (raw_k == "q" or raw_k == "escape" or raw_k == "esc")
		local is_pagedown = (raw_k == "pagedown" or (raw_k == "d" and key.ctrl) or (raw_k == "f" and key.ctrl))
		local is_pageup = (raw_k == "pageup" or (raw_k == "u" and key.ctrl) or (raw_k == "b" and key.ctrl))
		local is_home = (raw_k == "home" or raw_k == "g")
		local is_end = (raw_k == "end" or raw_k == "G")

		if is_down then
			if cur_idx < #files then
				local next_file = files[cur_idx + 1]
				state.active_file = next_file
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "arrow", "1" }, function() end)
					end)
				end
				return true
			end
		elseif is_up then
			if cur_idx > 1 then
				local prev_file = files[cur_idx - 1]
				state.active_file = prev_file
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "arrow", "-1" }, function() end)
					end)
				end
				return true
			end
		elseif is_pagedown then
			local step = math.max(1, math.floor(((cx.rows or 22) - 8) / 2))
			local next_idx = math.min(#files, cur_idx + step)
			if next_idx ~= cur_idx and files[next_idx] then
				state.active_file = files[next_idx]
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "arrow", tostring(next_idx - cur_idx) }, function() end)
					end)
				end
			end
			return true
		elseif is_pageup then
			local step = math.max(1, math.floor(((cx.rows or 22) - 8) / 2))
			local prev_idx = math.max(1, cur_idx - step)
			if prev_idx ~= cur_idx and files[prev_idx] then
				state.active_file = files[prev_idx]
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "arrow", tostring(prev_idx - cur_idx) }, function() end)
					end)
				end
			end
			return true
		elseif is_home then
			if #files > 0 and cur_idx > 1 then
				state.active_file = files[1]
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "arrow", tostring(1 - cur_idx) }, function() end)
					end)
				end
			end
			return true
		elseif is_end then
			if #files > 0 and cur_idx < #files then
				state.active_file = files[#files]
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "arrow", tostring(#files - cur_idx) }, function() end)
					end)
				end
			end
			return true
		elseif is_left then
			local parent_dir = cur_cwd:match("(.+)/[^/]+$") or "/"
			state.optimistic_cwd = parent_dir
			state.active_file = basename(cur_cwd)
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", cid, "leave" }, function() end)
				end)
			end
			return true
		elseif is_right or is_enter then
			if current_name and cur_cwd then
				local full_path = join_path(cur_cwd, current_name)
				local ok_dir = pcall(tern.fs.list, full_path)
				if ok_dir then
					state.optimistic_cwd = full_path
					state.active_file = nil
					if cid then
						pcall(function()
							tern.process.run({ "ya", "emit-to", cid, "cd", full_path }, function() end)
						end)
					end
					return true
				end
			end
			if is_enter then
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "open" }, function() end)
					end)
				end
			else
				if cid then
					pcall(function()
						tern.process.run({ "ya", "emit-to", cid, "enter" }, function() end)
					end)
				end
			end
			return true
		elseif is_space then
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", cid, "toggle" }, function() end)
				end)
			end
			return true
		elseif raw_k == "v" then
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", cid, "visual_mode" }, function() end)
				end)
			end
			return true
		elseif raw_k == "y" then
			if current_name and yz and yz.cwd then
				local full_path = join_path(yz.cwd, current_name)
				cx:copy(full_path)
				cx:toast("success", "Yanked path", full_path)
			end
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", cid, "yank" }, function() end)
				end)
			end
			return true
		elseif raw_k == "o" then
			if current_name and yz and yz.cwd then
				local full_path = yz.cwd .. "/" .. current_name
				cx:open(full_path)
				cx:toast("info", "Opening file in editor", full_path)
			end
			return true
		elseif is_quit then
			cx:exit(0)
			return true
		elseif raw_k == "r" and key.ctrl then
			return true
		end

		return false
	end,

	save = function(state)
		return nil
	end,
})
