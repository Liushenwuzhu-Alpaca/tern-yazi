-- Native component construction from prepared snapshots and preview payloads.
local ui = tern.ui
local path = require('../shared/path')
local model = require('../backend/model')
local format = require('./format')
local basename, join_path = path.basename, path.join_path
local fmt_size, file_icon = format.fmt_size, format.file_icon
local compute_parent, compute_directory = model.compute_parent, model.compute_directory
local file_is_dir = model.file_is_dir
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

local function selection_mark(paths: { [string]: boolean }, path: string): any
	return ui.span(paths[path] and "✓ " or "  ", paths[path] and "success" or "muted")
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
		table.insert(children, clickable(ui.el("button", { key = "go" }, { ui.text("Go") }), "path_submit"))
		table.insert(children, clickable(ui.el("button", { key = "cancel" }, { ui.text("Cancel") }), "path_cancel"))
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
			table.insert(ancestors, clickable(ui.el("button", { key = "cancel" }, { ui.text("Cancel [Esc]") }), "path_cancel"))
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
local function build_parent_column(yz: YaziState?, cur_cwd: string, max_lines: number, selected_paths: { [string]: boolean }): Node
	local parent_files, parent_cwd, parent_dirs = compute_parent(yz, cur_cwd)
	if not parent_files then
		return ui.card(
			{ ui.span("Parent", "muted") },
			{ ui.text({ ui.span(cur_cwd == "/" and "(root)" or "Waiting for Yazi directory metadata...", "muted") }) }
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
		local is_dir = parent_dirs and parent_dirs[fname]
		local sym, tone = file_icon(fname, is_dir)
		table.insert(items, {
			name = fname,
			url = join_path(parent_cwd, fname),
			label = { selection_mark(selected_paths, join_path(parent_cwd, fname)), ui.span(sym .. " ", tone), ui.span(fname, is_cur and "strong" or nil) },
		})
	end

	local parent_title = (parent_cwd and basename(parent_cwd) ~= "") and basename(parent_cwd) or "/"
	local card = ui.card(
		{ ui.span(parent_title, "muted") },
		{ viewport_list(items, max_lines, "parent_list", selected) }
	)
	card.p.title = parent_cwd or "/"
	return card
end

-- Right Column child preview: Expand subfolder contents as a scrollable list
local function render_child_directory_preview(yz: YaziState, full_url: string, dir_name: string, max_lines: number, selected_paths: { [string]: boolean }): Node
	local folder = compute_directory(yz, full_url)
	local list = folder and folder.files
	if not list then
		return ui.card(
			{ ui.span("[" .. dir_name .. "]", "accent") },
			{ ui.text({ ui.span("Waiting for Yazi directory preview...", "muted") }) }
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
		local sub_path = join_path(full_url, cname)
		local sym, tone = file_icon(cname, folder.file_dirs and folder.file_dirs[cname])
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

local function render(state: any, cx: any, yz: YaziState?, prepared: any?, preview_data: any?): any
		if not yz or not state.live then
			return {
				main = ui.card(
					{ ui.span(yz and "Yazi is offline" or "Connect to Yazi", "warn") },
					{
						ui.text(state.token and "The managed backend stopped or is not responding. File actions are disabled."
							or "Explicit attach requires a running Yazi with the tern.yazi plugin; checking actor acknowledgement."),
						ui.text(state.cid and ("Client " .. state.cid) or "No Yazi snapshot is available."),
						ui.text("Press q or Esc to close this panel."),
					}
				),
				dock = ui.row({
					clickable(ui.el("button", {}, { ui.text("Check Again") }), "poll"),
				}),
			}
		end

	local cur_cwd = prepared.cur_cwd
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
		input_bar.p = input_bar.p or {}
		input_bar.p.key = "input"
		input_bar.p.role = "yazi.input-row"
		input_bar.p.min = { h = "1.5lines" }
		input_bar.p.max = { h = "1.5lines" }
		table.insert(top_rows, input_bar)

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


		local file_list = prepared.file_list
		local selected_paths, marked_paths = prepared.selected_paths, prepared.marked_paths
		local active_name, cur_idx = prepared.active_name, prepared.cur_idx
		local active_full_url, is_active_dir = prepared.active_full_url, prepared.is_active_dir

		-- This is only the native list ceiling, not a terminal-to-list row conversion.
		-- The columns and their scrollers take the actual remaining pane height.
		local col_lines = math.max(1, cx.rows or 24)

		-- ---------------- Left Column (Parent Directory)
		local left_col_card = build_parent_column(yz, cur_cwd, col_lines, selected_paths)

		-- ---------------- Middle Column (Current Directory, scrollable)
		local middle_items = {}
		for _, fname in ipairs(file_list) do
			local is_active = (fname == active_name)
			local item_path = join_path(cur_cwd, fname)
			local sym, tone = file_icon(fname, file_is_dir(yz, cur_cwd, fname))
			table.insert(middle_items, {
				name = fname,
				url = item_path,
				label = { selection_mark(selected_paths, item_path), ui.span(marked_paths[item_path] and (yz.mode == "unset" and "- " or "+ ") or "", "warn"), ui.span(sym .. " ", tone), fname },
				tone = is_active and "accent" or nil,
			})
		end

		local middle_title = string.format("%s (%d/%d)", basename(cur_cwd), cur_idx, #file_list)
		local middle_col_card = ui.card(
			{ ui.span(middle_title, "strong") },
			{ #middle_items > 0 and viewport_list(middle_items, col_lines, "files_list", cur_idx)
				or ui.text({ ui.span(cur_cwd == yz.cwd and "(empty directory)" or "Waiting for Yazi directory metadata...", "muted") }) }
		)

		-- ---------------- Right Column (Preview / Child Subfolder Expansion)
		local right_col_card
		if not active_name then
			right_col_card = ui.card(
				{ ui.span("Preview", "muted") },
				{ ui.text({ ui.span(cur_cwd == yz.cwd and "(no file selected)" or "Waiting for Yazi directory metadata...", "muted") }) }
			)
		elseif is_active_dir then
			-- Child Directory Content Expansion (exactly like Yazi right pane!)
			right_col_card = render_child_directory_preview(yz, active_full_url, active_name, col_lines, selected_paths)
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
			left_col_card,
			middle_col_card,
			right_col_card,
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
			ui.badge(string.format("%d sel", yz.selected or 0), (yz.selected or 0) > 0 and "warn" or "muted"),
			ui.node("text", { role = "yazi.status-name", spans = { ui.span(active_name or "", "strong") } }),
			ui.text({ ui.span(" " .. pct_str .. " ", "muted") }),
			ui.badge(pos_str, "accent"),
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

		return {
			main = ui.col({ ui.node("col", { role = "yazi.browser", key = "browser", gap = "sm" }, top_rows) }),
			-- Region roots are stripped; preserve the row as a child of the dock.
			dock = ui.col({ status_bar }),
			layer = trash_layer or path_layer,
		}

end

M.render = render
return M
