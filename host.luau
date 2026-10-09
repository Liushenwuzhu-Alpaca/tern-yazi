-- host.luau: Host half of tern-yazi plugin.
-- Registers the "companion" block, reads Yazi state from File Inbox,
-- and renders an interactive UI tree.

local ui = tern.ui

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

type YaziState = {
	seq: number,
	client_id: any,
	cwd: string,
	files: { string },
	selected: number,
	hovered: HoverInfo?,
	tasks: TaskInfo?,
}

-- ---------------------------------------------------------------- helpers

local function clickable(node, act: string)
	local tbl = node :: any
	local p = tbl.props or {}
	p.action = act
	tbl.props = p
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
	local newest_seq = -1

	for _, fname in ipairs(entries) do
		if fname:match("^state%-.+%.json$") then
			local full_path = sdir .. "/" .. fname
			local ok_read, content = pcall(tern.fs.read, full_path, 256 * 1024)
			if ok_read and content then
				local ok_json, parsed = pcall(tern.json.decode, content)
				if ok_json and type(parsed) == "table" then
					local seq = parsed.seq or 0
					if seq > newest_seq then
						newest_seq = seq
						newest_content = parsed
					end
				end
			end
		end
	end
	return newest_content
end

-- ---------------------------------------------------------------- block

tern.block.define("companion", {
	init = function(cx, args, saved)
		return { seq = -1 }
	end,

	view = function(state, cx)
		local yz = read_latest_state()
		if not yz then
			return {
				main = ui.card(
					ui.text({ ui.span("Waiting for Yazi...", "muted") }),
					{
						ui.text("No active Yazi instance detected."),
						ui.text({
							ui.span("Launch "),
							ui.span("yazi", "strong"),
							ui.span(" with tern.yazi plugin installed."),
						}),
					}
				),
				dock = ui.row({
					clickable(ui.el("button", {}, { ui.text("Check Again") }), "poll"),
				}),
			}
		end

		local main_children = {}

		-- 1. Header with CWD and status badges
		local head_row = ui.row({
			ui.text({ ui.span("Path: ", "muted"), ui.span(yz.cwd or "/", "strong") }),
			ui.badge(string.format("%d selected", yz.selected or 0), (yz.selected or 0) > 0 and "accent" or "muted"),
			ui.badge("Synced", "success"),
		})
		table.insert(main_children, head_row)

		-- 2. Task progress if any tasks are running
		if yz.tasks and yz.tasks.total and yz.tasks.total > 0 then
			local t = yz.tasks
			local done = (t.succ or 0) + (t.fail or 0)
			local running = t.total - done
			if running > 0 then
				local ratio = t.total > 0 and (done / t.total) or 0
				local task_card = ui.card(
					ui.row({
						ui.badge("Tasks", "warn"),
						ui.text(string.format("%d active / %d total", running, t.total)),
					}),
					{
						ui.progress(ratio, string.format("%d of %d done", done, t.total)),
					}
				)
				table.insert(main_children, task_card)
			end
		end

		-- 3. File entries list
		local items = {}
		local hovered_url = yz.hovered and yz.hovered.url
		local hovered_name = hovered_url and basename(hovered_url) or nil

		for i, fname in ipairs(yz.files or {}) do
			local is_hovered = (fname == hovered_name) or (fname == hovered_url)
			local prefix = is_hovered and "❯ " or "  "
			local row_el = clickable(
				ui.row({
					ui.text({
						ui.span(prefix, is_hovered and "accent" or "muted"),
						ui.span(fname, is_hovered and "strong" or nil),
					}),
				}),
				"reveal=" .. fname
			)
			table.insert(items, row_el)
			if #items >= 30 then
				break
			end
		end

		local list_head = ui.row({
			ui.text("Directory Entries"),
			ui.badge(string.format("%d items", #(yz.files or {})), "neutral"),
		})
		local list_card = ui.card(list_head, { ui.col(items) })
		table.insert(main_children, list_card)

		-- 4. File Inspector Card
		if yz.hovered and yz.hovered.url then
			local h = yz.hovered
			local name = basename(h.url)
			local is_dir = h.dir or false
			local kv_node = ui.kv({
				{ ui.text("Name"), ui.text({ ui.span(name, "strong") }) },
				{ ui.text("Type"), ui.text(is_dir and "Directory" or "File") },
				{ ui.text("Size"), ui.text(fmt_size(h.size)) },
				{ ui.text("Modified"), ui.text(fmt_time(h.mtime)) },
				{ ui.text("Location"), ui.text({ ui.span(h.url or "-", "muted") }) },
			})

			local acts = ui.row({
				clickable(ui.el("button", {}, { ui.text("Copy Path") }), "copy_path=" .. (h.url or "")),
				clickable(ui.el("button", {}, { ui.text("Terminal Here") }), "terminal_here=" .. (yz.cwd or "")),
				clickable(ui.el("button", {}, { ui.text("Reveal in Yazi") }), "reveal=" .. name),
			})

			local inspector_card = ui.card(
				ui.row({ ui.text("File Inspector") }),
				{ kv_node, acts }
			)
			table.insert(main_children, inspector_card)
		end

		return {
			main = ui.col(main_children),
			dock = ui.row({
				clickable(ui.el("button", {}, { ui.text("Refresh") }), "poll"),
			}),
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

		if act == "copy_path" and val then
			cx:copy(val)
			cx:toast("success", "Path copied to clipboard", val)
		elseif act == "reveal" and val then
			local yz = read_latest_state()
			local cid = yz and yz.client_id
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", tostring(cid), "reveal", val }, function() end)
				end)
			end
		elseif act == "cd" and val then
			local yz = read_latest_state()
			local cid = yz and yz.client_id
			if cid then
				pcall(function()
					tern.process.run({ "ya", "emit-to", tostring(cid), "cd", val }, function() end)
				end)
			end
		elseif act == "terminal_here" and val then
			cx:toast("info", "Opening terminal in " .. val, nil)
			pcall(function()
				tern.process.run({ "tern", "split", "down", "--cwd", val }, function() end)
			end)
		elseif act == "poll" then
			-- Handled by view re-run
		end
	end,

	key = function(state, key, cx)
		if key.name == "r" and key.ctrl then
			return true
		end
		return false
	end,

	save = function(state)
		return nil
	end,
})
