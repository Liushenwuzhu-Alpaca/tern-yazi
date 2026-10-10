-- Native component construction from prepared snapshots and preview payloads.
local ui = tern.ui
local path = require('../shared/path')
local model = require('../backend/model')
local format = require('./format')
local theme = require('./theme')
local basename, join_path = path.basename, path.join_path
local fmt_size, file_icon, virtual_icon = format.fmt_size, format.file_icon, format.virtual_icon
local fmt_permissions, preview_kind = format.fmt_permissions, format.preview_kind
local compute_parent, compute_directory = model.compute_parent, model.compute_directory
type YaziState = model.YaziState
local M = {}

local LIST_IDS = {
	parent_list = "main.browser.columns.parent.parent_list",
	files_list = "main.browser.columns.current.files_list",
	child_list = "main.browser.columns.preview.child_list",
}

local function clickable(node: any, act: string): any
	node.p = node.p or {}
	node.p.actions = { click = act }
	return node
end

local function build_rich_preview(data: any, active_name: string, cx: any): Node
	if data.tag == "settings-unavailable" then
		return ui.text({ ui.span("Preview settings unavailable", "warn") })
	elseif data.tag == "unavailable" then
		return ui.node("col", { role = "yazi.preview-unavailable", gap = "none" }, {
			ui.text({ ui.span("Preview unavailable (" .. fmt_size(data.limit) .. " limit)", "warn") }),
		})
	end
	local ext = data.ext
	-- 0. Compressed Archive Preview (.zip, .tar, .tar.gz, .tgz, .7z, etc.)
	if data.tag == "archive" then
		local cached = data.items
		if type(cached) == "table" and #cached > 0 then
			local entry_nodes = {}
			for i = 1, math.min(18, #cached) do
				local item_name = cached[i]
				local is_sub_dir = (item_name:sub(-1) == "/")
				local sym, tone = virtual_icon(item_name, is_sub_dir)
				table.insert(entry_nodes, ui.text({
					ui.span(sym, tone),
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

	local content = data.content
	-- 1. SVG and Image Native Preview (via cx:blob + ui.node("image", ...))
	if data.tag == "image" then
		if #content == 0 then
			return ui.text({ ui.span("Image file is empty", "muted") })
		end
		local blob_id = data.blob
		if not blob_id then
			return ui.text({ ui.span("Image preview unavailable", "warn") })
		end
		return ui.node("col", { role = "yazi.image-preview", gap = "sm" }, {
			ui.node("image", {
				blob = blob_id,
				alt = active_name,
				role = "yazi.image",
				max = { w = 1, h = tostring(math.max(1, (cx.rows or 24) - 12)) .. "lines" },
			}),
			ui.text({ ui.span(string.format("Image (%s, %s)", ext:upper(), fmt_size(#content)), "muted") }),
		})
	end

	-- 2. Mermaid diagram preview (.mermaid / .mmd)
	if data.tag == "mermaid" then
		return ui.md("```mermaid\n" .. content .. "\n```")
	end

	-- 3. Markdown rich formatted document preview (.md / .markdown)
	if data.tag == "markdown" then
		return ui.md(content)
	end

	-- 4. Code / Text syntax highlighting preview
	if data.tag == "binary" then
		return ui.col({
			ui.text({ ui.span("Binary file (preview suppressed)", "muted") }),
			ui.text({ ui.span("Size: " .. fmt_size(#content), "strong") }),
		})
	end
	return ui.code(content, data.lang)
end


local function selection_tray(selected_paths: { [string]: boolean }): Node?
	local paths = {}
	for selected_path, selected in pairs(selected_paths) do
		if selected then table.insert(paths, selected_path) end
	end
	if #paths == 0 then return nil end
	table.sort(paths)
	local chips = {}
	for index = 1, math.min(6, #paths) do
		local selected_path = paths[index]
		table.insert(chips, ui.node("text", {
			key = selected_path, role = "yazi.selection-chip", title = selected_path,
			text = basename(selected_path), wrap = "none", truncate = "end",
		}))
	end
	if #paths > 6 then
		table.insert(chips, ui.node("text", {
			key = "overflow", role = "yazi.selection-overflow", text = "+" .. tostring(#paths - 6),
		}))
	end
	return ui.node("row", { key = "selection", role = "yazi.selection-tray", gap = "xs" }, chips)
end


local EMPTY_PATHS = {}

-- Every native list frame retains complete identities and real filenames.
-- Cached heavy row presentation and its spans are immutable after construction.
local HYDRATE_ORDER = { "files_list", "parent_list", "child_list" }
local HYDRATE_BATCH = 256
local HYDRATE_BUDGET_MS = 8
local LIST_OVERSCAN = 6

local function queue_row(cache: any, index: number)
	if cache.pending[index] then return end
	cache.pending[index] = true
	cache.pending_count += 1
	table.insert(cache.queue, index)
end

local function hydrate_row(cache: any, index: number): boolean
	if not cache.pending[index] then return false end
	cache.pending[index] = nil
	cache.pending_count -= 1
	local name, url = cache.files[index], cache.urls[index]
	local icon = cache.overrides and cache.overrides[name]
	if icon == nil then icon = cache.icons and cache.icons[name] end
	local is_dir = cache.dirs and cache.dirs[name]
	if is_dir == nil and cache.hovered_url == url then is_dir = cache.hovered_dir end
	local sym, tone = file_icon(icon, name, is_dir)
	local selected = cache.selected_paths and cache.selected_paths[url] == true or false
	local mark = cache.marked_paths and cache.marked_paths[url] and (cache.mode == "unset" and "- " or "+ ") or ""
	local active = cache.key == "parent_list" and name == cache.selected_name
	local previous = cache.items[index]
	if previous and previous.sym == sym and previous.icon_tone == tone
		and previous.selected == selected and previous.mark == mark and previous.active == active then
		return false
	end
	local labels = { ui.span(selected and "✓ " or "  ", selected and "success" or "muted") }
	if cache.key == "files_list" then table.insert(labels, ui.span(mark, "warn")) end
	table.insert(labels, ui.span(cache.key == "parent_list" and cache.layout == "editorial" and "" or sym, tone))
	table.insert(labels, ui.span(name, cache.key == "parent_list" and active and "strong" or nil))
	if cache.layout == "amber" then table.insert(labels, 1, ui.span(string.format("%02d ", index), "muted")) end
	local child = cache.children[index]
	if not child then
		child = {
			k = "item",
			p = {
				key = name,
				title = url,
				label = labels,
			},
		}
		cache.children[index] = child
	else
		child.p.label = labels
	end
	cache.items[index] = { sym = sym, icon_tone = tone, selected = selected, mark = mark, active = active }
	return true
end

local function finish_rows(cache: any)
	if cache.pending_count == 0 then cache.queue, cache.queue_head = {}, 1 end
end

local function hydration_pending(state: any): boolean
	for _, key in ipairs(HYDRATE_ORDER) do
		local cache = state.active_list_caches and state.active_list_caches[key]
		if cache and cache.pending_count > 0 then return true end
	end
	return false
end

-- Called only by the owning host's finite timer, using the latest visible caches.
local function hydrate(state: any): boolean
	local deadline, count, changed = tern.now() + HYDRATE_BUDGET_MS, 0, false
	for _, key in ipairs(HYDRATE_ORDER) do
		local cache = state.active_list_caches and state.active_list_caches[key]
		if cache then
			while cache.pending_count > 0 and cache.queue_head <= #cache.queue
				and count < HYDRATE_BATCH and tern.now() < deadline do
				local index = cache.queue[cache.queue_head]
				cache.queue_head += 1
				if cache.pending[index] then
					if hydrate_row(cache, index) then changed = true end
					count += 1
				end
			end
			finish_rows(cache)
		end
		if count >= HYDRATE_BATCH or tern.now() >= deadline then break end
	end
	return changed
end

local function directory_list(state: any, folder: any, cwd: string, max_lines: number, key: string,
	selected_name: string?, selected_paths: any, marked_paths: any, mode: string?, layout: string): Node
	local files = folder.files
	local caches = state.list_cache or {}
	state.list_cache = caches
	local cache = caches[key]
	local rebuild = not cache or cache.files ~= files or cache.cwd ~= cwd or cache.layout ~= layout
	if rebuild then
		cache = { files = files, cwd = cwd, layout = layout, key = key, indices = {}, names_by_url = {},
			urls = {}, items = {}, children = {}, pending = {}, pending_count = 0, queue = {}, queue_head = 1 }
		for index, name in ipairs(files) do
			local url = join_path(cwd, name)
			cache.indices[name], cache.names_by_url[url], cache.urls[index] = index, name, url
			queue_row(cache, index)
		end
		caches[key] = cache
	end
	state.active_list_caches[key] = cache
	local function dirty_name(name: string?)
		local index = name and cache.indices[name]
		if index then queue_row(cache, index) end
	end
	local function named_changes(previous: any, current: any, urls: boolean?)
		if previous == current then return end
		previous, current = previous or EMPTY_PATHS, current or EMPTY_PATHS
		for name, value in pairs(previous) do
			if value ~= current[name] then
				if urls then dirty_name(cache.names_by_url[name]) else dirty_name(name) end
			end
		end
		for name, value in pairs(current) do
			if value ~= previous[name] then
				if urls then dirty_name(cache.names_by_url[name]) else dirty_name(name) end
			end
		end
	end
	local hovered_url = folder.hovered and folder.hovered.url
	local hovered_dir = folder.hovered and folder.hovered.dir
	if not rebuild then
		named_changes(cache.dirs, folder.file_dirs)
		named_changes(cache.icons, folder.file_icons)
		named_changes(cache.overrides, folder.icon_overrides)
		named_changes(cache.selected_paths, selected_paths, true)
		named_changes(cache.marked_paths, marked_paths, true)
		if cache.mode ~= mode then
			for url in pairs(marked_paths or EMPTY_PATHS) do dirty_name(cache.names_by_url[url]) end
		end
		if cache.selected_name ~= selected_name then
			dirty_name(cache.selected_name)
			dirty_name(selected_name)
		end
		if cache.hovered_url ~= hovered_url or cache.hovered_dir ~= hovered_dir then
			dirty_name(cache.hovered_url and cache.names_by_url[cache.hovered_url])
			dirty_name(hovered_url and cache.names_by_url[hovered_url])
		end
	end
	cache.dirs, cache.icons, cache.overrides = folder.file_dirs, folder.file_icons, folder.icon_overrides
	cache.selected_paths, cache.marked_paths, cache.mode = selected_paths, marked_paths, mode
	cache.selected_name = selected_name
	cache.hovered_url, cache.hovered_dir = hovered_url, hovered_dir
	local selected_index = selected_name and cache.indices[selected_name] or 1
	-- Native reveal uses the least scroll distance, placing the cursor at either
	-- edge. Cover both possible viewports plus its six-row overscan, cursor first.
	-- The public list API has no scroll-offset event; wheel-only rows keep their
	-- real filenames until the bounded background queue supplies their details.
	local radius = math.ceil(max_lines) + LIST_OVERSCAN
	if #files > 0 then hydrate_row(cache, selected_index) end
	for distance = 1, math.min(radius, math.max(selected_index - 1, #files - selected_index)) do
		if selected_index - distance >= 1 then hydrate_row(cache, selected_index - distance) end
		if selected_index + distance <= #files then hydrate_row(cache, selected_index + distance) end
	end
	finish_rows(cache)
	local function get_child(i: number)
		local child = cache.children[i]
		if not child then
			local name, url = cache.files[i], cache.urls[i]
			child = {
				k = "item",
				p = {
					key = name,
					title = url,
					label = { { t = name } },
				},
			}
			cache.children[i] = child
		end
		return child
	end
	local children
	if #files <= 150 then
		children = table.create(#files)
		for i = 1, #files do
			children[i] = get_child(i)
		end
	else
		local win_size = 120
		local half = math.floor(win_size / 2)
		local start_idx = math.max(1, selected_index - half)
		local end_idx = math.min(#files, start_idx + win_size - 1)
		start_idx = math.max(1, end_idx - win_size + 1)
		children = table.create(end_idx - start_idx + 1)
		local pos = 1
		for i = start_idx, end_idx do
			children[pos] = get_child(i)
			pos += 1
		end
	end
	return {
		k = "list",
		p = {
			key = key,
			max = { lines = max_lines },
			selected = selected_name and cache.indices[selected_name]
				and (LIST_IDS[key] .. "." .. selected_name) or nil,
		},
		c = children,
	}
end

local function path_segments(cwd: string): { any }
	local home = tern.getenv("HOME") or ""
	local at_home = home ~= "" and (cwd == home or cwd:sub(1, #home + 1) == home .. "/")
	local root = at_home and home or "/"
	local segments = { { label = at_home and "~" or "/", path = root } }
	local tail = at_home and cwd:sub(#home + 1) or cwd
	local path = root
	for name in tail:gmatch("[^/]+") do
		path = join_path(path, name)
		table.insert(segments, { label = name, path = path })
	end
	return segments
end

local function path_header(state: any, cwd: string, cx: any): (any, any)
	local children, layer = {}, nil
	if state.path_mode then
		table.insert(children, ui.node("input", { key = "field", role = "yazi.path-input",
			text = state.path_query or "", cursor = state.path_cursor, anchor = state.path_anchor,
			placeholder = "Directory path", sendable = true, grow = 1, min = { w = 0 } }))
		table.insert(children, clickable(ui.el("button", { key = "go", role = "yazi.path-go" }, { ui.text("Go") }), "path_submit"))
		table.insert(children, clickable(ui.el("button", { key = "cancel", role = "yazi.path-cancel" }, { ui.text("Cancel") }), "path_cancel"))
	else
		local segments = path_segments(cwd)
		local budget = math.max(8, (cx.cols or 80) - 12)
		local cap = math.max(4, math.min(24, math.floor(budget / 2)))
		local first, used = #segments, math.min(cap, utf8.len(segments[#segments].label) or cap) + 5
		while first > 2 do
			local cost = math.min(cap, utf8.len(segments[first - 1].label) or cap) + 3
			if used + cost > budget then break end
			used += cost
			first -= 1
		end
		local function crumb(segment, index)
			return clickable(ui.el("button", { key = "dir" .. index, role = "yazi.path-segment",
				title = segment.path, min = { w = 0 }, max = { w = tostring(cap) .. "ch" } }, {
				ui.node("text", { text = segment.label, wrap = "none", truncate = "end" }),
			}), "path_cd:" .. segment.path)
		end
		table.insert(children, crumb(segments[1], 1))
		if first > 2 then
			table.insert(children, clickable(ui.el("button", { key = "more", role = "yazi.path-more",
				title = "Show ancestor directories" }, { ui.text("…") }), "path_more"))
		end
		for index = math.max(2, first), #segments do
			table.insert(children, ui.node("text", { key = "sep" .. index, text = "›", tone = "muted", shrink = 0 }))
			table.insert(children, crumb(segments[index], index))
		end
		-- The tail remains a distinct target even when every segment is truncated.
		table.insert(children, clickable(ui.el("button", { key = "edit", role = "yazi.path-edit",
			title = "Edit directory path", grow = 1, shrink = 0, min = { w = "3ch" } }, { ui.text(" ") }), "path_edit"))
		if state.path_menu then
			local ancestors = {}
			for index = 2, first - 1 do table.insert(ancestors, crumb(segments[index], index)) end
			table.insert(ancestors, clickable(ui.el("button", { key = "cancel", role = "yazi.path-cancel" }, { ui.text("Cancel [Esc]") }), "path_cancel"))
			layer = ui.col({ ui.node("card", { key = "ancestors", role = "yazi.path-ancestors" }, {
				ui.text("Ancestor directories"), ui.col(ancestors),
			}) })
		end
	end
	return ui.node("card", { key = "path", role = "yazi.path-header", title = cwd }, {
		ui.node("row", { key = "content", role = "yazi.path-content", gap = "xs" }, children),
	}), layer
end

-- ---------------------------------------------------------------- Miller columns builders

-- Left Column: Parent directory as a scrollable list; the current
-- directory row is the list selection (native active capsule).
local function build_parent_column(state: any, yz: YaziState?, cur_cwd: string, max_lines: number, selected_paths: { [string]: boolean }, layout: string): Node
	local parent_files, parent_cwd = compute_parent(yz, cur_cwd)
	if not parent_files then
		return ui.card(
			{ ui.span("Parent", "muted") },
			{ ui.text({ ui.span(cur_cwd == "/" and "(root)" or "Waiting for Yazi directory metadata...", "muted") }) }
		)
	end

	local folder = compute_directory(yz, parent_cwd)
	local list = directory_list(state, folder, parent_cwd, max_lines, "parent_list", basename(cur_cwd), selected_paths, EMPTY_PATHS, nil, layout)

	local parent_title = (parent_cwd and basename(parent_cwd) ~= "") and basename(parent_cwd) or "/"
	local card = { k = "card", p = { head = { ui.span(parent_title, "muted") } }, c = { list } }
	card.p.title = parent_cwd or "/"
	return card
end

-- Right Column child preview: Expand subfolder contents as a scrollable list
local function render_child_directory_preview(state: any, yz: YaziState, full_url: string, dir_name: string, max_lines: number, selected_paths: { [string]: boolean }, layout: string): Node
	local folder = compute_directory(yz, full_url)
	local list = folder and folder.files
	local count = folder and folder.file_count
	local heading = { ui.span(dir_name, "strong"), ui.span(count ~= nil
		and string.format(" · %d %s", count, count == 1 and "entry" or "entries") or " · — entries", "muted") }
	if not list or count == nil and #list == 0 then
		return ui.card(heading, { ui.text({ ui.span("Waiting for Yazi directory preview...", "muted") }) })
	end
	if count == 0 then
		return ui.card(heading, { ui.text({ ui.span("(empty directory)", "muted") }) })
	end
	local node = directory_list(state, folder, full_url, max_lines, "child_list", list[1], selected_paths, EMPTY_PATHS, nil, layout)
	return { k = "card", p = { head = heading }, c = { node } }
end

local function render(state: any, cx: any, yz: YaziState?, prepared: any?, preview_data: any?): any
	state.active_list_caches = {}
		if state.snapshot_error or (state.snapshot_waiting and not yz) then
			return theme.regions({ main = ui.card(
				{ ui.span(state.snapshot_error and "Yazi snapshot unavailable" or "Waiting for Yazi snapshot", "warn") },
				{ ui.text(state.snapshot_error or "The listing and cursor snapshots are changing; waiting for one matching generation."),
					ui.text("File actions are disabled until the complete snapshot is available. Press q or Esc to close.") }
			) }, state, cx)
		end
		if not yz or not state.live then
			return theme.regions({
				main = ui.card(
					{ ui.span(state.startup_error and "Cannot start Yazi" or ((yz or state.token) and "Yazi is offline"
						or (state.owner_kind == "standalone" and "Starting Yazi" or "Connect to Yazi")), "warn") },
					{
						ui.text(state.startup_error or (state.token and "The managed backend stopped or is not responding. File actions are disabled."
							or (state.owner_kind == "standalone" and "Waiting for the real Yazi backend and native attachment."
								or "Explicit attach requires a running Yazi with the tern.yazi plugin; checking actor acknowledgement."))),
						ui.text(state.cid and ("Client " .. state.cid) or "No Yazi snapshot is available."),
						ui.text("Press q or Esc to close this panel."),
					}
				),
				dock = ui.row({
					clickable(ui.el("button", {}, { ui.text("Check Again") }), "poll"),
				}),
			}, state, cx)
		end

	local cur_cwd = prepared.cur_cwd
	local layout = state.layout_theme or "current"
		local top_rows = {}
		local header, path_layer = path_header(state, cur_cwd, cx)
		table.insert(top_rows, header)

		-- Keep one input line reserved so editors do not shift the file columns.
		local input_bar = ui.row({ ui.text(" ") })
		if state.cmd_mode then
			input_bar = ui.row({
				ui.badge(state.cmd_block and "Shell [:]" or "Shell [;]", "warn"),
				ui.text({
					ui.span(" " .. (state.cmd_block and ":" or ";") .. (state.cmd_query or ""), "strong"),
					ui.span(state.cmd_mode and " ▎" or "", "warn"),
				}),
				clickable(ui.badge("Cancel [Esc]", "muted"), "clear_cmd"),
			})
		elseif state.find_mode then
			input_bar = ui.row({
				ui.badge(state.find_previous and "Find [?]" or "Find [/]", "accent"),
				ui.text({ ui.span(" " .. (state.find_query or ""), "strong"), ui.span(" ▎", "accent") }),
				clickable(ui.badge("Cancel [Esc]", "muted"), "clear_find"),
			})
		elseif state.filter_mode then
			input_bar = ui.row({
				ui.badge("Filter [f]", "accent"),
				ui.text({
					ui.span(" " .. (state.filter_query or ""), "strong"),
					ui.span(state.filter_mode and " ▎" or "", "accent"),
				}),
				clickable(ui.badge("Clear [Esc]", "muted"), "clear_filter"),
			})
		end
		local input_kind = state.cmd_mode and "shell" or (state.find_mode and "find" or (state.filter_mode and "filter" or nil))
		if input_kind then
			-- Presentation hooks keep the existing widgets, positional keys and actions.
			for index, role in ipairs({ "yazi.command-label", "yazi.command-bubble." .. input_kind, "yazi.command-cancel" }) do
				local part = input_bar.c[index]
				part.p = part.p or {}
				part.p.role = role
			end
		end
		input_bar.p = input_bar.p or {}
		input_bar.p.key = "input"
		input_bar.p.role = "yazi.input-row"
		input_bar.p.min = { h = "1.5lines" }
		input_bar.p.max = { h = "1.5lines" }
		table.insert(top_rows, input_bar)

		local file_list = prepared.file_list
		local selected_paths, marked_paths = prepared.selected_paths, prepared.marked_paths
		local active_name, cur_idx = prepared.active_name, prepared.cur_idx
		local active_full_url, is_active_dir = prepared.active_full_url, prepared.is_active_dir
		local active_hovered = prepared.active_hovered
		local active_size = active_hovered and active_hovered.size or nil

		-- This is only the native list ceiling, not a terminal-to-list row conversion.
		-- The columns and their scrollers take the actual remaining pane height.
		local col_lines = math.max(1, cx.rows or 24)

		-- ---------------- Middle Column (Current Directory, scrollable)
		local current_folder = compute_directory(yz, cur_cwd)
		local middle_list = #file_list > 0 and directory_list(state, current_folder, cur_cwd, col_lines,
			"files_list", active_name, selected_paths, marked_paths, yz.mode, layout)

		-- ---------------- Left Column (Parent Directory)
		local left_col_card = build_parent_column(state, yz, cur_cwd, col_lines, selected_paths, layout)

		local middle_title = string.format("%s (%d/%d)", basename(cur_cwd), cur_idx, #file_list)
		local middle_col_card = ui.card({ ui.span(middle_title, "strong") }, {
			middle_list or ui.text({ ui.span(cur_cwd == yz.cwd and "(empty directory)" or "Waiting for Yazi directory metadata...", "muted") }),
		})

		-- ---------------- Right Column (Preview / Child Subfolder Expansion)
		local right_col_card
		if not active_name then
			right_col_card = ui.card(
				{ ui.span("Preview", "muted") },
				{ ui.text({ ui.span(cur_cwd == yz.cwd and "(no file selected)" or "Waiting for Yazi directory metadata...", "muted") }) }
			)
		elseif is_active_dir then
			-- Child Directory Content Expansion (exactly like Yazi right pane!)
			right_col_card = render_child_directory_preview(state, yz, active_full_url, active_name, col_lines, selected_paths, layout)
		elseif is_active_dir == nil then
			right_col_card = ui.card({ ui.span(active_name, "strong") }, {
				ui.text({ ui.span("Waiting for Yazi file metadata...", "muted") }),
			})
		else
			-- Rich File Preview (Code, Markdown, Image, Archive table of contents)
			local preview_content = build_rich_preview(preview_data, active_name, cx)
			local act_buttons = {
				clickable(ui.el("button", {}, { ui.text("Open in Tern") }), "open_file=" .. active_full_url),
			}
			table.insert(act_buttons, clickable(ui.el("button", {}, { ui.text("Copy Path") }), "copy_path=" .. active_full_url))
			table.insert(act_buttons, clickable(ui.el("button", {}, { ui.text("Terminal Here") }), "terminal_here=" .. cur_cwd))

			right_col_card = ui.card(
				{ ui.span(active_name, "strong") },
				{
					ui.node("row", { key = "metadata", role = "yazi.preview-metadata", gap = "xs", wrap = true }, {
						ui.text({ ui.span(preview_kind(active_name, preview_data), "muted") }),
						ui.node("text", { role = "yazi.preview-size", text = fmt_size(active_size) }),
						ui.node("text", { role = "yazi.preview-permissions", text = fmt_permissions(active_hovered and active_hovered.permissions), tone = "muted" }),
					}),
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
		middle_col_card.p.title = middle_title .. "\n" .. cur_cwd
		right_col_card.p.role = "yazi.column.preview"
		right_col_card.p.key = "preview"
		right_col_card.p.title = active_name and active_full_url or "Preview"
		local miller_columns = ui.node("row", { role = "yazi.columns", key = "columns" }, {
			left_col_card, middle_col_card, right_col_card,
		})
		table.insert(top_rows, miller_columns)

		-- ---------------- Bottom Status Bar (Yazi-style Pill Status Bar)
		local mode_str = state.cmd_mode and "CMD" or (state.find_mode and "FIND" or (state.filter_mode and "FILTER" or (yz.mode == "select" and "VIS" or (yz.mode == "unset" and "UNSET" or "NOR"))))
		local mode_tone = (state.cmd_mode or (yz.mode and yz.mode ~= "normal")) and "warn" or "accent"

		local pct_num = (#file_list > 0) and math.floor((cur_idx / #file_list) * 100) or 0
		local pct_str = string.format("%d%%", pct_num)
		local pos_str = string.format("%d/%d", cur_idx, math.max(1, #file_list))

		local status_bar = ui.node("row", { role = "yazi.status", gap = "xs" }, {
			ui.badge(mode_str, mode_tone),
			ui.node("text", { role = "yazi.status-size", text = fmt_size(active_size), tone = "muted" }),
			ui.node("text", { role = "yazi.status-name", spans = { ui.span(active_name or "", "strong") } }),
			ui.node("row", { role = "yazi.status-right", gap = "xs" }, {
				ui.node("text", { role = "yazi.status-permissions", text = fmt_permissions(active_hovered and active_hovered.permissions), tone = "muted" }),
				ui.text({ ui.span(" " .. pct_str .. " ", "muted") }),
				ui.badge(pos_str, "accent"),
			}),
		})
		local trash_layer = nil
		if state.trash then
			local rows = {}
			for i, path in ipairs(state.trash.paths) do rows[i] = { name = tostring(i), label = tern.json.encode(path) } end
			local list = ui.list(rows)
			list.p = list.p or {}
			list.p.key = "paths"
			list.p.max = { lines = math.max(4, (cx.rows or 24) - 8) }
			trash_layer = ui.col({ ui.node("card", { key = "trash", role = "yazi.trash-confirm" }, {
				ui.text("Move these exact paths to Trash? Cancel changes nothing."), list,
				ui.node("row", { key = "controls" }, {
					clickable(ui.el("button", { key = "cancel" }, { ui.text("Cancel [Esc]") }), "trash_cancel"),
					clickable(ui.el("button", { key = "confirm", tone = "warn" }, { ui.text("Move to Trash [Enter]") }), "trash_confirm"),
				}),
			}) })
		end
		local dock_rows = { status_bar }
		if layout == "amber" then
			local tray = selection_tray(selected_paths)
			if tray then table.insert(dock_rows, tray) end
		end

		return theme.regions({
			main = ui.col({
				ui.node("col", { role = "yazi.browser", key = "browser", gap = "sm" }, top_rows),
			}),
			-- Region roots are stripped; preserve the row as a child of the dock.
			dock = ui.col(dock_rows),
			layer = trash_layer or path_layer,
		}, state, cx)

end

M.render = render
M.hydrate = hydrate
M.hydration_pending = hydration_pending
return M
