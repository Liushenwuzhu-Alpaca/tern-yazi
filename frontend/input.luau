-- Native edit, key, click and optimistic controller state.
local path = require('../shared/path')
local model = require('../backend/model')
local actions = require('../backend/actions')
local basename, safe_cid, join_path = path.basename, path.safe_cid, path.join_path
local compute_parent, compute_file_list = model.compute_parent, model.compute_file_list
local compute_directory, file_is_dir = model.compute_directory, model.file_is_dir
type YaziState = model.YaziState
local M = {}

local LIST_IDS = {
	parent_list = "main.browser.columns.parent.parent_list",
	files_list = "main.browser.columns.current.files_list",
	child_list = "main.browser.columns.preview.child_list",
}

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
	if state.source_client ~= source or state.source_cwd ~= yz.cwd then state.pending_g = nil end
	if state.source_client ~= source or state.source_cwd ~= yz.cwd or state.source_hover ~= hovered then
		state.active_file = nil
		state.optimistic_cwd = nil
		state.source_client = source
		state.source_cwd = yz.cwd
		state.source_hover = hovered
	end
	-- Only pending filter mutations own the local query. An unrelated action may
	-- publish its new filter snapshot before its acknowledgement reaches us.
	if not state.filter_mode and not actions.pending_filter(state)
		and yz.filter ~= nil and (state.filter_source ~= source or state.filter_seq ~= yz.seq) then
		state.filter_query = type(yz.filter) == "string" and yz.filter or ""
		state.filter_source, state.filter_seq = source, yz.seq
	end
end

local function clear_filter(state: any, yz: any, cx: any)
	state.filter_mode = false
	state.filter_query = ""
	actions.request(state, yz, { op = "filter", query = "" })
end

local function clear_find(state: any, yz: any, cx: any)
	state.find_mode = false
	state.find_query = ""
	state.find_previous = false
	actions.action(state, yz, "escape", { find = true })
end

local function erase_codepoint(text: string): string
	if text == "" then return text end
	local i = #text
	while i > 1 and text:byte(i) >= 128 and text:byte(i) < 192 do i -= 1 end
	return text:sub(1, i - 1)
end

local function typed_text(key: any): string?
	if key.name == "paste" then return key.text end
	if key.ctrl or key.meta or key.alt then return nil end
	if key.text and key.text ~= "" then return key.text end
	if key.name == "space" then return " " end
	if #(key.name or "") == 1 then
		return key.shift and key.name:upper() or key.name
	end
	return nil
end

local function trash_decision(state: any, cx: any, confirm: boolean)
	local pending = state.trash
	state.trash = nil
	if confirm and pending then
		actions.request(state, { client_id = pending.cid }, { op = "trash_commit", paths = pending.paths }, function(reply)
			if reply.ok then cx:toast("info", "Trash queued in Yazi", "Completion is reported by Yazi tasks") end
		end)
	end
end

local PATH_FIELD = "main.browser.path.content.field"

local function utf16_size(text: string): number
	local size = 0
	for _, cp in utf8.codes(text) do size += cp > 0xFFFF and 2 or 1 end
	return size
end

local function byte_at_utf16(text: string, offset: number): number
	local size = 0
	for pos, cp in utf8.codes(text) do
		if size >= offset then return pos end
		size += cp > 0xFFFF and 2 or 1
	end
	return #text + 1
end

local function replace_path(state: any, text: string, first: number?, last: number?)
	local query = state.path_query or ""
	local cursor = state.path_cursor or utf16_size(query)
	local anchor = state.path_anchor or cursor
	first = first or math.min(cursor, anchor)
	last = last or math.max(cursor, anchor)
	state.path_query = query:sub(1, byte_at_utf16(query, first) - 1)
		.. text .. query:sub(byte_at_utf16(query, last))
	state.path_cursor = first + utf16_size(text)
	state.path_anchor = state.path_cursor
end

local function resolve_directory(query: string, cwd: string): string?
	if query == "" or query:find("[%z\r\n]") then return nil end
	local home = tern.getenv("HOME") or ""
	if query == "~" or query:sub(1, 2) == "~/" then
		if home == "" then return nil end
		query = home .. query:sub(2)
	end
	if query:sub(1, 1) ~= "/" then query = join_path(cwd, query) end
	-- Preserve '..' for the filesystem to resolve through symbolic links.
	return query
end

local function jump_directory(state: any, yz: any, cx: any, target: string)
	local query = state.path_query
	state.path_menu = false
	actions.request(state, yz, { op = "cd", target = target }, function(reply)
		if reply.ok then
			state.active_file = nil
			state.optimistic_cwd = nil
			if state.path_query == query then state.path_mode = false end
		end
	end)
end

local function submit_path(state: any, yz: any, cx: any)
	local target = resolve_directory(state.path_query or "", yz.cwd or "/")
	if not target then
		cx:toast("error", "Invalid directory", "Enter a directory path on one line")
		return
	end
	jump_directory(state, yz, cx, target)
end

local function handle_key(state: any, key: any, cx: any, yz: YaziState?): boolean
	local raw = key.name or ""
	local k = raw:lower()
	local escape = k == "escape" or k == "esc" or (key.ctrl and k == "[")
	local enter = k == "enter" or k == "return"
	local plain_g = k == "g" and not key.shift and raw == k and not key.ctrl and not key.alt and not key.meta
	local pending_g = state.pending_g == true
	state.pending_g = nil
	if not state.live then
		if (k == "q" or escape) and not key.ctrl and not key.alt and not key.meta then
			state.close(cx)
		else cx:toast("error", "Yazi is offline", "Reconnect before using file actions") end
		return true
	end
	if state.trash then
		if escape then trash_decision(state, cx, false)
		elseif enter then trash_decision(state, cx, true) end
		return true
	end
	if state.path_menu then
		if escape then state.path_menu = false end
		return true
	end
	if state.path_mode then
		if escape then state.path_mode = false
		elseif enter then submit_path(state, yz, cx)
		elseif key.ctrl and k == "a" then
			state.path_anchor = 0; state.path_cursor = utf16_size(state.path_query or "")
		elseif k == "backspace" or k == "delete" then
			local query = state.path_query or ""
			local cursor = state.path_cursor or utf16_size(query)
			local anchor = state.path_anchor or cursor
			if cursor ~= anchor then replace_path(state, "")
			elseif k == "backspace" and cursor > 0 then
				local prefix = query:sub(1, byte_at_utf16(query, cursor) - 1)
				replace_path(state, "", utf16_size(erase_codepoint(prefix)), cursor)
			elseif k == "delete" then
				local next_byte = utf8.offset(query, 2, byte_at_utf16(query, cursor)) or (#query + 1)
				replace_path(state, "", cursor, utf16_size(query:sub(1, next_byte - 1)))
			end
		elseif k == "home" then state.path_cursor = 0; state.path_anchor = 0
		elseif k == "end" then
			state.path_cursor = utf16_size(state.path_query or ""); state.path_anchor = state.path_cursor
		else
			local text = typed_text(key)
			if text then replace_path(state, text) else return false end
		end
		return true
	end
	if state.cmd_mode then
		if escape then state.cmd_mode = false; state.cmd_query = ""
		elseif enter then
			local query, block = state.cmd_query or "", state.cmd_block == true
			if query == "" then state.cmd_mode = false; return true end
			actions.request(state, yz, { op = "command", action = "shell", args = { query, block = block } }, function(reply)
				if not reply.ok then return end
				if block and not state.token then
					if not actions.open_shell(state, reply, query, block) then return end
				else
					cx:toast("info", "Shell queued in Yazi", "Task accepted; completion is reported by Yazi tasks")
				end
				if state.cmd_query == query and state.cmd_block == block then
					state.cmd_mode = false; state.cmd_query = ""
				end
			end)
		elseif k == "backspace" then state.cmd_query = erase_codepoint(state.cmd_query or "")
		else
			local text = typed_text(key)
			if text then state.cmd_query = (state.cmd_query or "") .. text end
		end
		return true
	end
	if state.find_mode then
		if escape then clear_find(state, yz, cx)
		elseif enter then
			local query, previous = state.find_query or "", state.find_previous == true
			state.find_mode = false
			state.find_query = ""
			state.active_file = nil
			actions.request(state, yz, { op = "find", query = query, previous = previous })
		else
			local query = state.find_query or ""
			if k == "backspace" then query = erase_codepoint(query)
			else local text = typed_text(key); if not text then return true end; query ..= text end
			state.find_query = query
			state.active_file = nil
			actions.request(state, yz, { op = "find", query = query, previous = state.find_previous == true })
		end
		return true
	end
	if state.filter_mode then
		if escape then clear_filter(state, yz, cx)
		elseif enter then
			state.filter_mode = false
			actions.request(state, yz, { op = "filter", query = state.filter_query or "", done = true })
		else
			local query = state.filter_query or ""
			if k == "backspace" then query = erase_codepoint(query)
			else local text = typed_text(key); if not text then return true end; query ..= text end
			state.filter_query = query
			state.active_file = nil
			actions.request(state, yz, { op = "filter", query = query, done = false })
		end
		return true
	end
	if key.alt or key.meta then return false end
	if escape then
		state.filter_mode = false
		actions.action(state, yz, "escape", {})
		return true
	end
	if key.ctrl then
		if k == "a" then actions.action(state, yz, "toggle_all", { state = "on" }); return true end
		if k == "r" then actions.action(state, yz, "toggle_all", {}); return true end
		if k ~= "d" and k ~= "u" and k ~= "f" and k ~= "b" then return false end
	end
	local upper = key.shift or raw ~= k
	local page_down = k == "page_down" or k == "pagedown" or (key.ctrl and (k == "d" or k == "f"))
	local page_up = k == "page_up" or k == "pageup" or (key.ctrl and (k == "u" or k == "b"))
	if page_down or page_up then
		local half = (key.ctrl and (k == "d" or k == "u")) or (not key.ctrl and key.shift)
		actions.action(state, yz, "arrow", { (page_up and "-" or "") .. (half and "50%" or "100%") })
		return true
	end
	if key.ctrl then return false end
	if k == "end" or (k == "g" and upper) then actions.action(state, yz, "arrow", { "bot" }); return true end
	if k == "home" then actions.action(state, yz, "arrow", { "top" }); return true end
	if plain_g then
		if pending_g then actions.action(state, yz, "arrow", { "top" })
		else state.pending_g = true end
		return true
	end
	if k == "n" then actions.action(state, yz, "find_arrow", { previous = upper }); return true end
	if upper and k:match("^[a-z]$") then
		if k == "v" then actions.action(state, yz, "visual_mode", { unset = true })
		elseif k == "h" then actions.action(state, yz, "back", {})
		elseif k == "l" then actions.action(state, yz, "forward", {})
		else return false end
		return true
	end
	if k == "j" or k == "down" or k == "arrowdown" or k == "arrow_down" or k == "k" or k == "up" or k == "arrowup" or k == "arrow_up" then
		local step = (k == "j" or k == "down" or k == "arrowdown" or k == "arrow_down") and 1 or -1
		actions.action(state, yz, "arrow", { tostring(step) })
	elseif k == "h" or k == "left" or k == "arrowleft" or k == "arrow_left" then
		actions.action(state, yz, "leave", {})
	elseif k == "l" or k == "right" or k == "arrowright" or k == "arrow_right" then
		actions.action(state, yz, "enter", {})
	elseif enter or k == "o" then
		local selected = yz and yz.selected_urls or {}
		if #selected > 0 then
			actions.open_selected(state, selected)
		elseif yz and yz.hovered and yz.hovered.url then
			if enter and yz.hovered.dir == true then actions.action(state, yz, "enter", {})
			else cx:open(yz.hovered.url) end
		end
	elseif k == "space" or k == " " then actions.request(state, yz, { op = "toggle_advance" })
	elseif k == "v" then actions.action(state, yz, "visual_mode", {})
	elseif k == "y" then actions.action(state, yz, "yank", {})
	elseif k == "x" then actions.action(state, yz, "yank", { cut = true })
	elseif k == "d" then
		actions.request(state, yz, { op = "trash_prepare" }, function(reply)
			if reply.ok and reply.paths and #reply.paths > 0 then
				state.trash = { cid = safe_cid(yz.client_id), paths = reply.paths }
			elseif reply.ok then cx:toast("info", "Nothing to trash", nil) end
		end)
	elseif k == "f" then state.filter_mode = true
	elseif k == "/" or k == "?" then
		state.find_mode = true
		state.find_query = ""
		state.find_previous = k == "?" or key.shift == true
	elseif (k == ";" or k == "semicolon") and not key.shift then state.cmd_mode = true; state.cmd_block = false; state.cmd_query = ""
	elseif k == ":" or k == "colon" or ((k == ";" or k == "semicolon") and key.shift) then state.cmd_mode = true; state.cmd_block = true; state.cmd_query = ""
	elseif k == "." or k == "period" then actions.action(state, yz, "hidden", { state = "toggle" })
	elseif k == "q" then state.close(cx)
	else return false end
	return true
end

local function event(state: any, ev: any, cx: any, yz: YaziState?)
	local act, val = ev.act, ev.value
	local cwd = yz and yz.cwd or ""
		if not state.live then
			cx:toast("error", "Yazi is offline", "Reconnect before using file actions")
			return
		end
		if state.trash then
			if act == "trash_confirm" then trash_decision(state, cx, true)
			elseif act == "trash_cancel" then trash_decision(state, cx, false) end
			return
		end
		state.pending_g = nil
		if act == "path_edit" then
			state.path_mode, state.path_menu = true, false
			state.cmd_mode, state.find_mode, state.filter_mode = false, false, false
			state.path_query = cwd
			state.path_cursor, state.path_anchor = utf16_size(cwd), 0
			return
		elseif act == "path_cancel" then
			state.path_mode, state.path_menu = false, false
			return
		elseif act == "path_more" then
			state.path_menu = not state.path_menu
			return
		elseif act == "path_submit" then
			if state.path_mode then submit_path(state, yz, cx) end
			return
		elseif type(act) == "string" and act:sub(1, 8) == "path_cd:" then
			jump_directory(state, yz, cx, act:sub(9))
			return
		elseif ev.id == PATH_FIELD and state.path_mode then
			if ev.ev == "focus" then
				-- Select-all belongs only to entering path mode. Native pointer
				-- editing owns the caret after the user clicks inside the field.
				state.path_cursor, state.path_anchor = nil, nil
				return
			end
			if ev.ev == "edit" then
				if ev.len ~= utf16_size(state.path_query or "") then return end
				replace_path(state, ev.text or "", ev.from, ev.to)
				state.path_cursor, state.path_anchor = ev.cursor, ev.cursor
			elseif ev.ev == "send" then
				state.path_query = ev.text or state.path_query
				submit_path(state, yz, cx)
			end
			return
		end

		if ev.ev == "select" or ev.ev == "activate" then
			local name = list_event_name(ev)
			local cur_cwd_ev = state.optimistic_cwd or cwd
			local base, names, dirs
			if ev.id == LIST_IDS.parent_list then
				names, base, dirs = compute_parent(yz, cur_cwd_ev)
			elseif ev.id == LIST_IDS.files_list and yz then
				base = cur_cwd_ev
				names = compute_file_list(state, yz, cur_cwd_ev)
				local folder = compute_directory(yz, base)
				dirs = folder and folder.file_dirs
			elseif ev.id == LIST_IDS.child_list then
				local active = state.active_file
					or (yz and yz.hovered and yz.hovered.url and basename(yz.hovered.url))
				if active then
					base = join_path(cur_cwd_ev, active)
					local folder = compute_directory(yz, base)
					names = folder and folder.files
					dirs = folder and folder.file_dirs
				end
			end
			if not name or not base or not names or not table.find(names, name) then
				return
			end

			local target = join_path(base, name)
			local is_dir = dirs and dirs[name]
			if type(is_dir) ~= "boolean" then is_dir = file_is_dir(yz, base, name) end
			if ev.ev == "activate" and is_dir == nil then return end
			if ev.ev == "activate" and is_dir then
				state.optimistic_cwd = target
				state.active_file = nil
				actions.request(state, yz, { op = "activate", target = target, action = "enter" })
			else
				if base ~= cwd then
					state.optimistic_cwd = base
				end
				state.active_file = name
				if ev.ev == "activate" then
					cx:open(target)
				else actions.action(state, yz, "reveal", { target, no_dummy = true }) end
			end
			return
		end

		if act == "copy_path" and val then
			cx:copy(val)
			cx:toast("success", "Path copied to clipboard", val)
		elseif act == "reveal" and val then
			state.active_file = val
			actions.action(state, yz, "reveal", { join_path(state.optimistic_cwd or cwd, val), no_dummy = true })
		elseif act == "enter_dir" and val then
			state.optimistic_cwd = val
			state.active_file = nil
			actions.request(state, yz, { op = "activate", target = val, action = "enter" })
		elseif act == "open_file" and val then
			cx:open(val)
		elseif act == "terminal_here" and val then
			actions.terminal_here(state, val)
		elseif act == "leave" then
			actions.action(state, yz, "leave", {})
		elseif act == "clear_filter" then
			clear_filter(state, yz, cx)
		elseif act == "clear_find" then
			clear_find(state, yz, cx)
		elseif act == "clear_cmd" then
			state.cmd_mode = false
			state.cmd_query = ""
		end

end

local function init(state: any)
	state.active_file = nil
	state.optimistic_cwd = nil
	state.filter_mode, state.filter_query = false, ""
	state.find_mode, state.find_query, state.find_previous = false, "", false
	state.cmd_mode, state.cmd_query, state.cmd_block = false, "", false
end

local function restore_command(state: any, block: boolean, query: string)
	state.cmd_mode, state.cmd_block, state.cmd_query = true, block, query
end

local function close(state: any)
	state.find_mode, state.find_query, state.find_previous = false, "", false
end

local function reset(state: any, clear_trash: boolean?)
	state.active_file, state.optimistic_cwd = nil, nil
	if clear_trash then state.trash = nil end
end

M.init = init
M.close = close
M.restore_command = restore_command
M.reset = reset
M.sync_hover = sync_hover
M.key = handle_key
M.event = event
return M
