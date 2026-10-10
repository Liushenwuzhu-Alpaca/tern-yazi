-- Window-local protocol state. Native presentation remains in frontend/window.
local inbox = require('./inbox')
local preview_policy = require('./preview_policy')
local state_dir = inbox.state_dir
local read_json = inbox.read_json
local lease_hint = inbox.lease_hint
local BLOCK_KIND = "tern-yazi.companion"

local managed = {}
local closed = {}
local shell_pending = {}
local files_pending = {}
local nonce_seq = 0
local nonce_epoch = tern.fs.read("/proc/sys/kernel/random/uuid", 64):match("^[%w-]+")
local shell_key = "shell_terminals"
local native_key = "native_shell_requests"
local preview_config_stamp = nil
local Window = {}

local function stop_session(record)
	closed[record.key] = true
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

local function shell_store()
	local value = tern.kv.get(shell_key)
	return type(value) == "table" and value or {}
end
local function native_store()
	local value = tern.kv.get(native_key)
	return type(value) == "table" and value or {}
end
local function remove_request(path)
	pcall(tern.fs.remove, path)
	pcall(tern.fs.remove, path .. ".ack")
end
local function shell_quote(text: string): string
	return "'" .. text:gsub("'", "'\\''") .. "'"
end
local function shell_event(body, act, nonce, err)
	return { pane = body.pane, event = { ev = "action", id = "main.poll", act = act,
		request_id = body.id, cid = body.cid, token = body.token, pane = body.pane, nonce = nonce,
		shell = body.shell, error = err } }
end
local function files_event(body, act, nonce, err)
	return { pane = body.pane, event = { ev = "action", id = "main.poll", act = act,
		request_id = body.id, cid = body.cid, token = body.token, pane = body.pane,
		owner_pane = body.owner_pane, nonce = nonce, paths = body.paths, error = err } }
end
local function same_paths(left, right): boolean
	if type(left) ~= "table" or type(right) ~= "table" or #left ~= #right then return false end
	for i, path in ipairs(left) do if right[i] ~= path then return false end end
	return true
end

-- The frontend consumes one discovery candidate at a time, presenting it
-- before reading the next health file or checking another session's lease.
function Window.discover_managed(panes)
	local sdir = state_dir()
	if not tern.fs.exists(sdir) then return nil end
	local ok, entries = pcall(tern.fs.list, sdir)
	if not ok then return nil end
	return { entries = entries, index = 0, seen = {}, panes = panes, dir = sdir }
end
function Window.next_managed(scan)
	local panes, sdir, seen = scan.panes, scan.dir, scan.seen
	while scan.index < #scan.entries do
		scan.index += 1
		local fname = scan.entries[scan.index]
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
		local adoption = false
		local launch = false
		if not record then
			nonce_seq += 1
			record = { cid = cid, token = health.token, key = key, owner_pane = health.owner_pane,
				cwd = health.cwd, nonce = nonce_epoch .. "-" .. tostring(nonce_seq), started = tern.now() }
			managed[cid] = record
			local token, hint = lease_hint(cid)
			if token == record.token and hint and not panes[hint] then
				-- Closing during reload must not resurrect the companion.
				stop_session(record)
				continue
			elseif token == record.token and hint and panes[hint] and panes[hint].block == BLOCK_KIND then
				record.pane = hint
				record.adopting = true
				adoption = true
			else launch = true end
		end
		return { record = record, health = health, adopt = adoption, launch = launch }
	end
	return nil
end

function Window.managed_launch_plan(record)
	return { owner_pane = record.owner_pane,
		args = { record.cid, record.token, tostring(record.owner_pane), record.cwd, record.nonce } }
end
function Window.managed_launched(record, pane)
	record.pane = pane
	record.started = tern.now()
	record.adopting = false
end
function Window.managed_active(candidate, query): boolean
	local record, health = candidate.record, candidate.health
	if record.health_seq ~= health.seq then record.health_seq = health.seq; record.health_seen = tern.now() end
	if record.stopped or tern.now() - record.health_seen >= 3000 then return false end
	if record.pane and not query.pane(record.pane) then stop_session(record); return false end
	return true
end
function Window.managed_lease(record)
	local token, hint, nonce = lease_hint(record.cid)
	if token == record.token and hint == record.pane and nonce == record.nonce then
		record.verified = true
	elseif tern.now() - record.started >= 3000 then
		if record.adopting then return "relaunch" end
		stop_session(record)
		return "failed"
	end
	return "poll"
end
function Window.managed_poll(record)
	return { pane = record.pane, event = { ev = "action", id = "main.poll", act = "managed_poll",
		cid = record.cid, token = record.token, nonce = record.nonce } }
end
function Window.finish_managed_scan(scan)
	local offline = {}
	for cid, record in pairs(managed) do
		if not scan.seen[cid] then
			-- Preserve explicit offline UI while stopping its lease.
			record.stopped = true
			if record.pane and scan.panes[record.pane] then table.insert(offline, record.pane)
			else managed[cid] = nil end
		end
	end
	return offline
end
function Window.focused_managed(focused, query)
	for _, record in pairs(managed) do
		if record.pane and query.pane(record.pane)
			and (record.pane == focused or record.owner_pane == focused) then return record end
	end
	return nil
end
function Window.managed_closed(pane)
	local restore = {}
	for cid, record in pairs(managed) do
		if record.pane == pane or record.owner_pane == pane then
			stop_session(record)
			if record.pane == pane then table.insert(restore, record); managed[cid] = nil end
		end
	end
	return restore
end
function Window.managed_request(req, query)
	local prefix = state_dir() .. "/"
	if req.path:sub(1, #prefix) ~= prefix then return nil end
	local cid = req.path:sub(#prefix + 1):match("^managed%-(%d+)%.json$")
	if not cid then return nil end
	local health = read_json(req.path)
	local live = health and health.client_id == cid and type(health.owner_pane) == "number"
		and query.pane(health.owner_pane)
	return { handled = true, wake = live and true or false }
end

function Window.preview_config_changed(): boolean
	local ok, stamp = pcall(function()
		return tern.json.encode(tern.kv.get("preview_limits") or tern.json.null)
	end)
	stamp = (ok and "ok:" or "error:") .. tostring(stamp)
	if stamp == preview_config_stamp then return false end
	preview_config_stamp = stamp
	return true
end
function Window.initialize_preview_limits()
	local defaults = preview_policy.defaults()
	local config = tern.kv.get("preview_limits")
	if config == nil then
		tern.kv.set("preview_limits", defaults)
	elseif type(config) == "table" then
		local changed = false
		for name, value in pairs(defaults) do
			if config[name] == nil then config[name] = value; changed = true end
		end
		if changed then tern.kv.set("preview_limits", config) end
	end
end

function Window.shell_request(req, query)
	local prefix = state_dir() .. "/shell-"
	if req.path:sub(1, #prefix) ~= prefix then return nil end
	local id = req.path:sub(#prefix + 1):match("^([%w-]+)%.json$")
	if not id then return nil end
	local body = read_json(req.path)
	local pane = body and type(body.pane) == "number" and query.pane(body.pane)
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
		remove_request(req.path)
		return { handled = true, unavailable = true }
	end
	local pending = shell_pending[id]
	if not pending then
		nonce_seq += 1
		pending = { nonce = nonce_epoch .. "-shell-" .. tostring(nonce_seq), body = body, started = tern.now() }
		shell_pending[id] = pending
		return { handled = true, send = shell_event(body, "shell_handoff_check", pending.nonce) }
	end
	if pending.body.shell.command ~= body.shell.command or pending.body.shell.cwd ~= body.shell.cwd
		or pending.body.pane ~= body.pane or pending.body.cid ~= body.cid or pending.body.token ~= body.token then
		shell_pending[id] = nil
		remove_request(req.path)
		return { handled = true, send = shell_event(pending.body, "shell_failed", pending.nonce,
			"Shell handoff changed during acknowledgement") }
	end
	local ack = read_json(req.path .. ".ack")
	if not ack or ack.id ~= id or ack.nonce ~= pending.nonce or ack.cid ~= body.cid
		or ack.token ~= body.token or ack.pane ~= body.pane or type(ack.shell) ~= "table"
		or ack.shell.command ~= body.shell.command or ack.shell.cwd ~= body.shell.cwd
		or ack.shell.block ~= body.shell.block then return { handled = true } end
	shell_pending[id] = nil
	remove_request(req.path)
	-- The completed display wrapper exits cleanly while keeping child status visible.
	local script = body.shell.command .. '\nstatus=$?\nprintf "\\n[exit %s] Press Enter to return to Yazi..." "$status"\nIFS= read -r answer\nexit 0'
	return { handled = true, launch = { body = body, nonce = pending.nonce,
		pane = body.pane, cwd = body.shell.cwd, command = "sh -c " .. shell_quote(script) } }
end
function Window.shell_completed(launch, terminal, was_zoomed, err)
	if not terminal then return shell_event(launch.body, "shell_failed", launch.nonce, err) end
	local store = shell_store()
	store[tostring(terminal)] = { pane = launch.body.pane, owner_pane = launch.body.owner_pane,
		was_zoomed = was_zoomed }
	tern.kv.set(shell_key, store)
	return shell_event(launch.body, "shell_launched", launch.nonce)
end

-- One acknowledged effect owns the frozen, case-distinct selection.
function Window.files_request(req, query)
	local prefix = state_dir() .. "/files-"
	if req.path:sub(1, #prefix) ~= prefix then return nil end
	local id = req.path:sub(#prefix + 1):match("^([%w-]+)%.json$")
	if not id then return nil end
	local body = read_json(req.path)
	local pane = body and type(body.pane) == "number" and query.pane(body.pane)
	-- Only the owning window consumes effects delivered to multiple windows.
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
			and health.owner_pane == body.owner_pane and query.pane(body.owner_pane)
			and token == body.token and hint == body.pane
	end
	local pending = files_pending[id]
	local function fail(message)
		files_pending[id] = nil
		remove_request(req.path)
		if pending then
			return { handled = true, send = files_event(pending.body, "files_failed", pending.nonce, message) }
		end
		return { handled = true, message = message }
	end
	if not valid then return fail("The originating Yazi session is unavailable") end
	local tab = query.tab_of(body.pane)
	if not tab then return fail("The originating Yazi pane is not in a tab") end
	if not pending then
		nonce_seq += 1
		pending = { nonce = nonce_epoch .. "-files-" .. tostring(nonce_seq), body = body, tab = tab }
		files_pending[id] = pending
		return { handled = true, send = files_event(body, "files_handoff_check", pending.nonce) }
	end
	if pending.body.pane ~= body.pane or pending.body.cid ~= body.cid or pending.body.token ~= body.token
		or pending.body.owner_pane ~= body.owner_pane or not same_paths(pending.body.paths, body.paths)
		or pending.tab ~= tab then return fail("Selected-file handoff changed during acknowledgement") end
	local ack = read_json(req.path .. ".ack")
	if not ack or ack.version ~= 1 or ack.id ~= id or ack.nonce ~= pending.nonce or ack.cid ~= body.cid
		or ack.token ~= body.token or ack.pane ~= body.pane or ack.owner_pane ~= body.owner_pane
		or not same_paths(ack.paths, body.paths) then return { handled = true } end
	files_pending[id] = nil
	remove_request(req.path)
	return { handled = true, batch = { pane = body.pane, paths = body.paths, body = body, nonce = pending.nonce } }
end
function Window.files_completed(batch, failures)
	return files_event(batch.body, #failures == 0 and "files_opened" or "files_failed", batch.nonce,
		#failures > 0 and table.concat(failures, "\n") or nil)
end

local function native_session_live(query, record)
	local health = read_json(state_dir() .. "/managed-" .. record.cid .. ".json")
	local token, pane = lease_hint(record.cid)
	local source = query.pane(record.pane)
	local owner = query.pane(record.owner_pane)
	local session = managed[record.cid]
	return health and health.client_id == record.cid and health.token == record.token
		and health.owner_pane == record.owner_pane and token == record.token and pane == record.pane
		and source and source.block == BLOCK_KIND and owner and session
		and session.token == record.token and session.pane == record.pane and not session.stopped
		and session.health_seen and tern.now() - session.health_seen < 3000
end
local function native_terminal_alive(query, record)
	local info = record.terminal and query.pane(record.terminal)
	local owned = record.terminal and shell_store()[tostring(record.terminal)]
	return info and info.kind == "terminal" and owned and owned.native_id == record.id
		and owned.cid == record.cid and owned.token == record.token
end

-- Persistent claims survive reload. Closed terminals remain tombstones until
-- the shim revokes their request; no route is permission to execute twice.
function Window.native_request(req, query)
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
			and native_session_live(query, body)
	end
	if not valid then return { handled = true } end
	local registry = native_store()
	local previous = registry[id]
	if previous then
		if previous.cid ~= body.cid or previous.token ~= body.token or previous.pane ~= body.pane
			or previous.owner_pane ~= body.owner_pane then return { handled = true } end
		if not native_terminal_alive(query, previous) then previous.closed = true; tern.kv.set(native_key, registry) end
		return { handled = true }
	end
	local stem = state_dir() .. "/native-shell-" .. id
	if tern.fs.exists(stem .. ".started") or tern.fs.exists(stem .. ".result") then
		registry[id] = { cid = body.cid, token = body.token, pane = body.pane,
			owner_pane = body.owner_pane, path = req.path, closed = true }
		tern.kv.set(native_key, registry)
		return { handled = true }
	end
	local record = { id = id, cid = body.cid, token = body.token, pane = body.pane,
		owner_pane = body.owner_pane, path = req.path, was_zoomed = query.zoomed(body.pane) }
	registry[id] = record
	tern.kv.set(native_key, registry)
	-- Transport quoting only; immutable private argv/env stay owned by the helper.
	return { handled = true, launch = { record = record, pane = body.pane, cwd = body.cwd,
		command = shell_quote(body.helper) .. " --native-terminal " .. shell_quote(req.path) } }
end
function Window.native_completed(launch, terminal)
	local record = launch.record
	if not terminal then record.closed = true
	else
		record.terminal = terminal
		local store = shell_store()
		store[tostring(terminal)] = { pane = record.pane, owner_pane = record.owner_pane,
			was_zoomed = record.was_zoomed, native_id = record.id, cid = record.cid, token = record.token }
		tern.kv.set(shell_key, store)
	end
	local registry = native_store()
	registry[record.id] = record
	tern.kv.set(native_key, registry)
end
function Window.expire_native_terminals(query, closed_pane)
	local registry = native_store()
	local close = {}
	for id, record in pairs(registry) do
		local alive = native_terminal_alive(query, record)
		if closed_pane and closed_pane == record.terminal then record.closed = true; alive = false end
		if alive and (closed_pane == record.pane or closed_pane == record.owner_pane
			or not native_session_live(query, record)) then
			table.insert(close, record.terminal)
			record.closed = true
		end
		if not alive and not tern.fs.exists(record.path) then registry[id] = nil end
	end
	tern.kv.set(native_key, registry)
	return close
end
function Window.shell_closed(pane)
	local store = shell_store()
	local previous = store[tostring(pane)]
	if not previous then return nil end
	store[tostring(pane)] = nil
	tern.kv.set(shell_key, store)
	return previous
end
function Window.expire_handoffs(query)
	local events = {}
	for id, pending in pairs(shell_pending) do
		if not query.pane(pending.body.pane) or tern.now() - pending.started >= 30000 then
			shell_pending[id] = nil
			remove_request(state_dir() .. "/shell-" .. id .. ".json")
			table.insert(events, shell_event(pending.body, "shell_failed", pending.nonce, "Shell terminal handoff expired"))
		end
	end
	-- Batch ACKs have no timer/retry; only host revocation or pane removal expires them.
	for id, pending in pairs(files_pending) do
		local path = state_dir() .. "/files-" .. id .. ".json"
		if not query.pane(pending.body.pane) or not tern.fs.exists(path) then
			files_pending[id] = nil
			remove_request(path)
		end
	end
	return events
end

return Window
