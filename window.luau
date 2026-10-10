-- window.luau: Window half of tern-yazi plugin.
-- Controls the "companion" block pane, manages PiP float/dock toggle,
-- and periodically pokes the block when state files change.

local PLUGIN_ID = "tern-yazi"
local BLOCK_KIND = PLUGIN_ID .. ".companion"
local DEFAULT_CORNER = "tr"

local companion = { pane = nil }
local managed = {}
local closed = {}
local nonce_seq = 0
local nonce_epoch = tern.fs.read("/proc/sys/kernel/random/uuid", 64):match("^[%w-]+")

-- Persist only restore intent; the actual split tree stays owned by Tern.
local overlay_key = "managed_overlays"
local function overlay_id(record): string
	return record.cid .. "-" .. record.token
end
local function overlay_store(): any
	local value = tern.kv.get(overlay_key)
	return type(value) == "table" and value or {}
end
local function zoom_pane(cx, pane, zoomed)
	local tab = cx.session:tab_of(pane)
	local layout = tab and cx.session:layout(tab)
	if not layout then return end
	cx.layout:focus(pane)
	if layout.zoomed ~= zoomed then cx:command("zoom") end
end
local function remember_overlay(cx, record)
	local store = overlay_store()
	local key = overlay_id(record)
	local saved = store[key]
	if type(saved) == "table" and saved.owner_pane == record.owner_pane then
		record.was_zoomed = saved.was_zoomed == true
	else
		local tab = cx.session:tab_of(record.owner_pane)
		local layout = tab and cx.session:layout(tab)
		record.was_zoomed = layout and layout.zoomed or false
		store[key] = { owner_pane = record.owner_pane, was_zoomed = record.was_zoomed }
		tern.kv.set(overlay_key, store)
	end
end
local function restore_overlay(cx, record)
	local owner = cx.session:pane(record.owner_pane)
	if owner then
		local focused = cx.session:focused()
		-- Do not steal focus from another client or a different tab.
		if focused == record.pane or focused == record.owner_pane then
			zoom_pane(cx, record.owner_pane, record.was_zoomed == true)
		end
	end
	local store = overlay_store()
	store[overlay_id(record)] = nil
	tern.kv.set(overlay_key, store)
end

local function state_dir(): string
	local dir = tern.getenv("XDG_RUNTIME_DIR")
	return dir and dir ~= "" and (dir .. "/tern-yazi") or "/tmp/tern-yazi"
end

local function read_json(path: string): any?
	local ok, text = pcall(tern.fs.read, path, 256 * 1024)
	if not ok then return nil end
	local decoded, value = pcall(tern.json.decode, text)
	return decoded and type(value) == "table" and value or nil
end

local function lease_hint(cid: string): (string?, number?, string?)
	local ok, text = pcall(tern.fs.read, state_dir() .. "/managed-" .. cid .. ".lease", 4096)
	if not ok then return nil, nil, nil end
	local token, pane, _, _, nonce = text:match("^(%S+)%s+(%d+)%s+(%d+)%s+(%d+)%s+(%S+)")
	if not token then token, pane = text:match("^(%S+)%s+(%d+)") end
	return token, tonumber(pane), nonce
end

local function stop_session(record)
	closed[record.cid .. "-" .. record.token] = true
	-- Never recreate private control files after the supervisor has removed them.
	local stem = state_dir() .. "/managed-" .. record.cid
	local health = read_json(stem .. ".json")
	if health and health.token == record.token then
		pcall(tern.fs.write, stem .. ".stop", record.token .. "\n")
		local token = lease_hint(record.cid)
		if token == record.token then pcall(tern.fs.write, stem .. ".lease", "") end
	end
	record.stopped = true
end

local function companion_alive(cx): boolean
	return companion.pane ~= nil and cx.session:pane(companion.pane) ~= nil
end

-- Ordinary Yazi is attach-only. A persisted snapshot is not evidence that its
-- process is alive, and cannot silently create a panel on startup.
local function open_companion(cx)
	if companion_alive(cx) then return end
	local pane = cx:new_block(BLOCK_KIND, nil, "beside", { focus = true })
	if not pane then tern.log.warn("new_block failed", BLOCK_KIND); return end
	companion.pane = pane
	local ok, err = cx.layout:float(pane, nil, DEFAULT_CORNER)
	if not ok then tern.log.warn("float failed", err) end
	cx.session:event(pane, { ev = "action", id = "main.poll", act = "poll" })
end

local function toggle(cx)
	local focused = cx.session:focused()
	local pane = companion_alive(cx) and companion.pane or nil
	for _, record in pairs(managed) do
		if record.pane and cx.session:pane(record.pane)
			and (record.pane == focused or record.owner_pane == focused) then
			zoom_pane(cx, record.pane, true)
			return
		end
	end
	if not pane then open_companion(cx); return end
	local info = cx.session:pane(pane)
	if info and info.floating then cx.layout:dock(pane)
	else
		local ok, err = cx.layout:float(pane, nil, DEFAULT_CORNER)
		if not ok then tern.log.warn("float failed", err) end
	end
end

local preview_config_stamp = nil
local function poke_preview_config(cx, panes)
	local ok, stamp = pcall(function()
		return tern.json.encode(tern.kv.get("preview_limits") or tern.json.null)
	end)
	stamp = (ok and "ok:" or "error:") .. tostring(stamp)
	if stamp == preview_config_stamp then return end
	preview_config_stamp = stamp
	for _, info in pairs(panes) do
		if info.block == BLOCK_KIND then
			cx.session:event(info.pane, { ev = "action", id = "main.poll", act = "poll" })
		end
	end
end

local function launch_managed(cx, record)
	-- new_block is relative to focus; explicitly choose the launching shell.
	cx.layout:focus(record.owner_pane)
	remember_overlay(cx, record)
	record.pane = cx:new_block(BLOCK_KIND,
		{ record.cid, record.token, tostring(record.owner_pane), record.cwd, record.nonce }, "beside", { focus = true })
	record.started = tern.now()
	record.adopting = false
	if record.pane then zoom_pane(cx, record.pane, true) end
end

local function poke(cx)
	local panes = {}
	for _, info in ipairs(cx.session:panes()) do panes[info.pane] = info end
	poke_preview_config(cx, panes)
	-- Poll every block, including saved unmanaged blocks recovered after reload.
	-- Hosts gate all file actions on their own pinned client's live evidence.
	for _, info in pairs(panes) do
		if info.block == BLOCK_KIND then
			cx.session:event(info.pane, { ev = "action", id = "main.poll", act = "poll" })
		end
	end
	local sdir = state_dir()
	if not tern.fs.exists(sdir) then return end
	local ok, entries = pcall(tern.fs.list, sdir)
	if not ok then return end
	local seen = {}
	for _, fname in ipairs(entries) do
		local cid = fname:match("^managed%-(%d+)%.json$")
		if not cid then continue end
		local health = read_json(sdir .. "/" .. fname)
		if not health or health.client_id ~= cid or type(health.token) ~= "string"
			or not health.token:match("^[%w-]+$") or type(health.owner_pane) ~= "number"
			or type(health.seq) ~= "number" or type(health.cwd) ~= "string" then continue end
		local key = cid .. "-" .. health.token
		local stopped_ok, stopped_token = pcall(tern.fs.read, sdir .. "/managed-" .. cid .. ".stop", 4096)
		if stopped_ok and stopped_token:match("^(%S+)") == health.token then closed[key] = true end
		seen[cid] = true
		local record = managed[cid]
		if record and record.token ~= health.token then record = nil; managed[cid] = nil end
		if closed[key] then continue end
		if not panes[health.owner_pane] then
			if record then stop_session(record) end
			continue
		end
		if not record then
			nonce_seq += 1
			record = { cid = cid, token = health.token, owner_pane = health.owner_pane,
				cwd = health.cwd, nonce = nonce_epoch .. "-" .. tostring(nonce_seq), started = tern.now() }
			managed[cid] = record
			local token, hint = lease_hint(cid)
			if token == record.token and hint and not panes[hint] then
				-- Closing during a window reload must not resurrect the backend UI.
				stop_session(record)
				continue
			elseif token == record.token and hint and panes[hint] and panes[hint].block == BLOCK_KIND then
				record.pane = hint
				record.adopting = true
				remember_overlay(cx, record)
			else launch_managed(cx, record) end
		end
		if record.health_seq ~= health.seq then record.health_seq = health.seq; record.health_seen = tern.now() end
		if record.stopped or tern.now() - record.health_seen >= 3000 then continue end
		if record.pane and not cx.session:pane(record.pane) then stop_session(record); continue end
		if not record.pane then launch_managed(cx, record) end
		if not record.pane then continue end
		local token, hint, nonce = lease_hint(cid)
		if token == record.token and hint == record.pane and nonce == record.nonce then
			record.verified = true
		elseif tern.now() - record.started >= 3000 then
			if record.adopting then
				-- A recycled hint never receives a lease for this client. Try one new
				-- correctly pinned block; never bind by "first companion pane".
				launch_managed(cx, record)
			else
				stop_session(record)
				cx:toast("error", "Cannot attach native Yazi", "The block did not acknowledge its client/token")
				continue
			end
		end
		cx.session:event(record.pane, { ev = "action", id = "main.poll", act = "managed_poll",
			cid = record.cid, token = record.token, nonce = record.nonce })
	end
	for cid, record in pairs(managed) do
		if not seen[cid] then
			-- Preserve the pane as an explicit offline UI, but stop its lease.
			record.stopped = true
			if record.pane and panes[record.pane] then
				cx.session:event(record.pane, { ev = "action", id = "main.poll", act = "poll" })
			else managed[cid] = nil end
		end
	end
end
-- A launcher file-open wakes an idle window without depending on a render timer.
tern.route.open(function(req, cx)
	local prefix = state_dir() .. "/"
	if req.path:sub(1, #prefix) ~= prefix then return nil end
	local cid = req.path:sub(#prefix + 1):match("^managed%-(%d+)%.json$")
	if not cid then return nil end
	local health = read_json(req.path)
	if not health or health.client_id ~= cid or type(health.owner_pane) ~= "number"
		or not cx.session:pane(health.owner_pane) then
		cx:toast("error", "Cannot open managed Yazi", "The launching pane or managed session is missing")
	else
		poke(cx)
	end
	return { handled = true }
end)

tern.on("window_start", function(cx) poke(cx) end)

tern.on("pane_closed", function(ev, cx)
	if companion.pane == ev.pane then companion.pane = nil end
	for cid, record in pairs(managed) do
		if record.pane == ev.pane or record.owner_pane == ev.pane then
			stop_session(record)
			if record.pane == ev.pane then restore_overlay(cx, record) end
			if record.pane == ev.pane then managed[cid] = nil end
		end
	end
end)

local shell_pending = {}
local shell_key = "shell_terminals"
local function shell_store()
	local value = tern.kv.get(shell_key)
	return type(value) == "table" and value or {}
end
local function shell_event(cx, body, act, nonce, err)
	if cx.session:pane(body.pane) then
		cx.session:event(body.pane, { ev = "action", id = "main.poll", act = act,
			request_id = body.id, cid = body.cid, token = body.token, pane = body.pane, nonce = nonce,
			shell = body.shell, error = err })
	end
end
local function shell_quote(text: string): string
	return "'" .. text:gsub("'", "'\\''") .. "'"
end
tern.route.open(function(req, cx)
	local prefix = state_dir() .. "/shell-"
	if req.path:sub(1, #prefix) ~= prefix then return nil end
	local id = req.path:sub(#prefix + 1):match("^([%w-]+)%.json$")
	if not id then return nil end
	local body = read_json(req.path)
	local pane = body and type(body.pane) == "number" and cx.session:pane(body.pane)
	local valid = body and body.version == 1 and body.id == id and pane and pane.block == BLOCK_KIND
		and type(body.cid) == "string" and body.cid:match("^%d+$")
		and id:match("%-" .. tostring(body.pane) .. "%-%d+$")
		and type(body.shell) == "table" and type(body.shell.command) == "string"
		and type(body.shell.cwd) == "string" and body.shell.cwd:sub(1, 1) == "/"
	if valid and body.token then
		local health = read_json(state_dir() .. "/managed-" .. body.cid .. ".json")
		local token, hint = lease_hint(body.cid)
		valid = health and health.token == body.token and health.owner_pane == body.owner_pane
			and token == body.token and hint == body.pane
	end
	if not valid then
		pcall(tern.fs.remove, req.path)
		pcall(tern.fs.remove, req.path .. ".ack")
		cx:toast("error", "Cannot open shell terminal", "The originating Yazi session is unavailable")
		return { handled = true }
	end
	local pending = shell_pending[id]
	if not pending then
		nonce_seq += 1
		pending = { nonce = nonce_epoch .. "-shell-" .. tostring(nonce_seq), body = body, started = tern.now() }
		shell_pending[id] = pending
		shell_event(cx, body, "shell_handoff_check", pending.nonce)
		return { handled = true }
	end
	if pending.body.shell.command ~= body.shell.command or pending.body.shell.cwd ~= body.shell.cwd
		or pending.body.pane ~= body.pane or pending.body.cid ~= body.cid or pending.body.token ~= body.token then
		shell_pending[id] = nil
		pcall(tern.fs.remove, req.path)
		pcall(tern.fs.remove, req.path .. ".ack")
		shell_event(cx, pending.body, "shell_failed", pending.nonce, "Shell handoff changed during acknowledgement")
		return { handled = true }
	end
	local ack = read_json(req.path .. ".ack")
	if not ack or ack.id ~= id or ack.nonce ~= pending.nonce or ack.cid ~= body.cid
		or ack.token ~= body.token or ack.pane ~= body.pane or type(ack.shell) ~= "table"
		or ack.shell.command ~= body.shell.command or ack.shell.cwd ~= body.shell.cwd
		or ack.shell.block ~= body.shell.block then return { handled = true } end
	shell_pending[id] = nil
	pcall(tern.fs.remove, req.path)
	pcall(tern.fs.remove, req.path .. ".ack")
	-- The child's status stays visible; the completed display wrapper exits cleanly.
	local script = body.shell.command .. '\nstatus=$?\nprintf "\\n[exit %s] Press Enter to return to Yazi..." "$status"\nIFS= read -r answer\nexit 0'
	local source_tab = cx.session:tab_of(body.pane)
	local source_layout = source_tab and cx.session:layout(source_tab)
	local source_zoomed = source_layout and source_layout.zoomed or false
	local ok, terminal = pcall(function()
		return cx.layout:split(body.pane, "right", { cwd = body.shell.cwd,
			command = "sh -c " .. shell_quote(script) }, { focus = true })
	end)
	if not ok or not terminal then
		shell_event(cx, body, "shell_failed", pending.nonce, tostring(terminal or "Terminal launch failed"))
	else
		local store = shell_store()
		store[tostring(terminal)] = { pane = body.pane, owner_pane = body.owner_pane,
			was_zoomed = source_zoomed }
		tern.kv.set(shell_key, store)
		zoom_pane(cx, terminal, true)
		shell_event(cx, body, "shell_launched", pending.nonce)
	end
	return { handled = true }
end)

-- One host effect carries the frozen selection; native window opens must not
-- compete as independent link effects for the focused pane.
local files_pending = {}
local function same_paths(left, right): boolean
	if type(left) ~= "table" or type(right) ~= "table" or #left ~= #right then return false end
	for i, path in ipairs(left) do if right[i] ~= path then return false end end
	return true
end
local function files_event(cx, body, act, nonce, err)
	if cx.session:pane(body.pane) then
		cx.session:event(body.pane, { ev = "action", id = "main.poll", act = act,
			request_id = body.id, cid = body.cid, token = body.token, pane = body.pane,
			owner_pane = body.owner_pane, nonce = nonce, paths = body.paths, error = err })
	end
end
tern.route.open(function(req, cx)
	local prefix = state_dir() .. "/files-"
	if req.path:sub(1, #prefix) ~= prefix then return nil end
	local id = req.path:sub(#prefix + 1):match("^([%w-]+)%.json$")
	if not id then return nil end
	local body = read_json(req.path)
	local pane = body and type(body.pane) == "number" and cx.session:pane(body.pane)
	-- Effects may reach multiple windows; only the owning window consumes them.
	if not pane then return { handled = true } end
	local valid = req.origin == "link" and body.version == 1 and body.id == id and pane.block == BLOCK_KIND
		and type(body.cid) == "string" and body.cid:match("^%d+$")
		and id:match("%-" .. tostring(body.pane) .. "%-%d+$")
		and type(body.paths) == "table" and #body.paths > 0
	if valid then
		for _, path in ipairs(body.paths) do
			if type(path) ~= "string" or path:sub(1, 1) ~= "/" or path:find("%z") then valid = false; break end
		end
	end
	if valid and body.token then
		local health = read_json(state_dir() .. "/managed-" .. body.cid .. ".json")
		local token, hint = lease_hint(body.cid)
		valid = health and health.client_id == body.cid and health.token == body.token
			and health.owner_pane == body.owner_pane and cx.session:pane(body.owner_pane)
			and token == body.token and hint == body.pane
	end
	local pending = files_pending[id]
	local function fail(message)
		files_pending[id] = nil
		pcall(tern.fs.remove, req.path)
		pcall(tern.fs.remove, req.path .. ".ack")
		if pending then files_event(cx, pending.body, "files_failed", pending.nonce, message)
		else cx:toast("error", "Cannot open selected files", message) end
		return { handled = true }
	end
	if not valid then return fail("The originating Yazi session is unavailable") end
	local tab = cx.session:tab_of(body.pane)
	if not tab then return fail("The originating Yazi pane is not in a tab") end
	if not pending then
		nonce_seq += 1
		pending = { nonce = nonce_epoch .. "-files-" .. tostring(nonce_seq), body = body, tab = tab }
		files_pending[id] = pending
		files_event(cx, body, "files_handoff_check", pending.nonce)
		return { handled = true }
	end
	if pending.body.pane ~= body.pane or pending.body.cid ~= body.cid or pending.body.token ~= body.token
		or pending.body.owner_pane ~= body.owner_pane or not same_paths(pending.body.paths, body.paths)
		or pending.tab ~= tab then return fail("Selected-file handoff changed during acknowledgement") end
	local ack = read_json(req.path .. ".ack")
	if not ack or ack.version ~= 1 or ack.id ~= id or ack.nonce ~= pending.nonce or ack.cid ~= body.cid
		or ack.token ~= body.token or ack.pane ~= body.pane or ack.owner_pane ~= body.owner_pane
		or not same_paths(ack.paths, body.paths) then return { handled = true } end
	files_pending[id] = nil
	pcall(tern.fs.remove, req.path)
	pcall(tern.fs.remove, req.path .. ".ack")
	-- Keep every split anchored to this pane in its tab, including case-distinct
	-- paths; native opens bypass routes and never re-read hover or selection.
	zoom_pane(cx, body.pane, false)
	local failures = {}
	for index, path in ipairs(body.paths) do
		local ok, err = pcall(function() cx:open(path, "beside", { focus = index == #body.paths }) end)
		if not ok then table.insert(failures, path .. ": " .. tostring(err)) end
	end
	files_event(cx, body, #failures == 0 and "files_opened" or "files_failed", pending.nonce,
		#failures > 0 and table.concat(failures, "\n") or nil)
	return { handled = true }
end)

-- Keep request identities across window reloads. A closed terminal is a
-- tombstone until the shim revokes its request, never permission to rerun it.
local native_key = "native_shell_requests"
local function native_store()
	local value = tern.kv.get(native_key)
	return type(value) == "table" and value or {}
end

local function native_session_live(cx, record)
	local health = read_json(state_dir() .. "/managed-" .. record.cid .. ".json")
	local token, pane = lease_hint(record.cid)
	local source = cx.session:pane(record.pane)
	local owner = cx.session:pane(record.owner_pane)
	local session = managed[record.cid]
	return health and health.client_id == record.cid and health.token == record.token
		and health.owner_pane == record.owner_pane and token == record.token and pane == record.pane
		and source and source.block == BLOCK_KIND and owner and session
		and session.token == record.token and session.pane == record.pane and not session.stopped
		and session.health_seen and tern.now() - session.health_seen < 3000
end

local function native_terminal_alive(cx, record)
	local info = record.terminal and cx.session:pane(record.terminal)
	local owned = record.terminal and shell_store()[tostring(record.terminal)]
	return info and info.kind == "terminal" and owned and owned.native_id == record.id
		and owned.cid == record.cid and owned.token == record.token
end

tern.route.open(function(req, cx)
	local prefix = state_dir() .. "/native-shell-"
	if req.path:sub(1, #prefix) ~= prefix then return nil end
	local id = req.path:sub(#prefix + 1):match("^([%w-]+)%.json$")
	if not id then return nil end
	local body = read_json(req.path)
	local valid = req.origin == "link" and body and body.version == 1 and body.id == id
		and type(body.cid) == "string" and body.cid:match("^%d+$")
		and type(body.token) == "string" and body.token:match("^[%w-]+$")
		and type(body.pane) == "number" and type(body.owner_pane) == "number"
		and type(body.helper) == "string" and body.helper:sub(1, 1) == "/" and not body.helper:find("%z")
		and type(body.cwd) == "string" and body.cwd:sub(1, 1) == "/"
		and type(body.argv) == "table" and type(body.env) == "table" and type(body.wait) == "boolean"
	if valid then
		local identity = body.cid .. "-" .. body.token .. "-"
		valid = id:sub(1, #identity) == identity and id:sub(#identity + 1):match("^%d+$")
			and native_session_live(cx, body)
	end
	if not valid then return { handled = true } end
	local registry = native_store()
	local previous = registry[id]
	if previous then
		if previous.cid ~= body.cid or previous.token ~= body.token or previous.pane ~= body.pane
			or previous.owner_pane ~= body.owner_pane then return { handled = true } end
		if not native_terminal_alive(cx, previous) then
			previous.closed = true
			tern.kv.set(native_key, registry)
		end
		-- An absent/exited pane is a tombstone, not permission to relaunch.
		return { handled = true }
	end
	local stem = state_dir() .. "/native-shell-" .. id
	if tern.fs.exists(stem .. ".started") or tern.fs.exists(stem .. ".result") then
		registry[id] = { cid = body.cid, token = body.token, pane = body.pane,
			owner_pane = body.owner_pane, path = req.path, closed = true }
		tern.kv.set(native_key, registry)
		return { handled = true }
	end
	local source_tab = cx.session:tab_of(body.pane)
	local source_layout = source_tab and cx.session:layout(source_tab)
	local record = { id = id, cid = body.cid, token = body.token, pane = body.pane,
		owner_pane = body.owner_pane, path = req.path,
		was_zoomed = source_layout and source_layout.zoomed or false }
	registry[id] = record
	tern.kv.set(native_key, registry)
	-- Only transport quoting: /bin/sh argv and the expanded native command stay
	-- private in the immutable request and are executed by the Rust helper.
	local ok, terminal = pcall(function()
		return cx.layout:split(body.pane, "right", { cwd = body.cwd,
			command = shell_quote(body.helper) .. " --native-terminal " .. shell_quote(req.path) }, { focus = true })
	end)
	if not ok or not terminal then
		record.closed = true
		cx:toast("error", "Cannot open native Yazi terminal", "Terminal launch failed; the native request will expire")
	else
		record.terminal = terminal
		local store = shell_store()
		store[tostring(terminal)] = { pane = body.pane, owner_pane = body.owner_pane,
			was_zoomed = record.was_zoomed, native_id = id, cid = body.cid, token = body.token }
		tern.kv.set(shell_key, store)
		zoom_pane(cx, terminal, true)
	end
	registry[id] = record
	tern.kv.set(native_key, registry)
	return { handled = true }
end)

local function expire_native_terminals(cx, closed_pane)
	local registry = native_store()
	local close = {}
	for id, record in pairs(registry) do
		local alive = native_terminal_alive(cx, record)
		if closed_pane and closed_pane == record.terminal then record.closed = true; alive = false end
		if alive and (closed_pane == record.pane or closed_pane == record.owner_pane
			or not native_session_live(cx, record)) then
			table.insert(close, record.terminal)
			record.closed = true
		end
		if not alive and not tern.fs.exists(record.path) then registry[id] = nil end
	end
	tern.kv.set(native_key, registry)
	for _, pane in ipairs(close) do cx.layout:close(pane) end
end

tern.on("pane_closed", function(ev, cx) expire_native_terminals(cx, ev.pane) end)

tern.on("pane_closed", function(ev, cx)
	local store = shell_store()
	local previous = store[tostring(ev.pane)]
	if not previous then return end
	store[tostring(ev.pane)] = nil
	tern.kv.set(shell_key, store)
	local target = cx.session:pane(previous.pane) and previous.pane or previous.owner_pane
	local focused = cx.session:focused()
	if target and cx.session:pane(target) and (focused == ev.pane or focused == target) then
		zoom_pane(cx, target, previous.was_zoomed == true)
		if target == previous.pane then cx.session:event(target, { ev = "action", id = "main.poll", act = "poll" }) end
	end
end)

local function expire_shell_handoffs(cx)
	for id, pending in pairs(shell_pending) do
		if not cx.session:pane(pending.body.pane) or tern.now() - pending.started >= 30000 then
			shell_pending[id] = nil
			local path = state_dir() .. "/shell-" .. id .. ".json"
			pcall(tern.fs.remove, path)
			pcall(tern.fs.remove, path .. ".ack")
			shell_event(cx, pending.body, "shell_failed", pending.nonce, "Shell terminal handoff expired")
		end
	end
	-- Batch ACKs have no timer or retry. Revoke only requests already removed
	-- by their host lifecycle, or whose originating pane has disappeared.
	for id, pending in pairs(files_pending) do
		local path = state_dir() .. "/files-" .. id .. ".json"
		if not cx.session:pane(pending.body.pane) or not tern.fs.exists(path) then
			files_pending[id] = nil
			pcall(tern.fs.remove, path)
			pcall(tern.fs.remove, path .. ".ack")
		end
	end
end

local function arm_timer()
	tern.timer(500, function(cx)
		if cx then
			expire_shell_handoffs(cx)
			local ok, err = pcall(poke, cx)
			if not ok then tern.log.warn("Yazi managed poll failed", tostring(err)) end
			expire_native_terminals(cx)
		end
		arm_timer()
	end)
end
arm_timer()

tern.command({ id = "toggle", title = "Yazi: Toggle Companion Panel", group = "Yazi", run = toggle })
tern.bind("ctrl+alt+y", toggle)

tern.command({
	id = "preview_limits",
	title = "Yazi: Configure Preview Limits",
	group = "Yazi",
	run = function(cx)
		local ok, err = pcall(function()
			local defaults = {
				image_bytes = 16 * 1024 * 1024,
				markdown_bytes = 1024 * 1024,
				code_bytes = 1024 * 1024,
				mermaid_bytes = 256 * 1024,
			}
			local config = tern.kv.get("preview_limits")
			if config == nil then
				tern.kv.set("preview_limits", defaults)
			elseif type(config) == "table" then
				local changed = false
				for name, value in pairs(defaults) do
					if config[name] == nil then
						config[name] = value
						changed = true
					end
				end
				if changed then tern.kv.set("preview_limits", config) end
			end
		end)
		if not ok then
			cx:toast("error", "Cannot initialize preview limits", tostring(err))
		end
		cx:open(tern.plugin.data .. "/kv.json")
		cx:toast("info", "Edit preview_limits and save to apply", "Positive integer bytes; caps: image 16 MiB, Markdown/code 4 MiB, Mermaid 1 MiB")
	end,
})
