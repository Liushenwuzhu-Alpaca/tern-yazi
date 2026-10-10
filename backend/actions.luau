-- Actor requests and token-bound native handoffs; completion is plain events.
local path = require('../shared/path')
local inbox = require('./inbox')
local session = require('./session')
local safe_cid = path.safe_cid
local state_dir, read_json = inbox.state_dir, inbox.read_json
local refresh_live = session.refresh_live
local M = {}

local function notify(state: any, level: string, title: string, detail: any?)
	state.on_event({ kind = "toast", level = level, title = title, detail = detail })
end

local function route(state: any, path: string)
	state.on_event({ kind = "open", path = path })
end

local function restore_command(state: any, handoff: any)
	state.on_event({ kind = "restore_command", block = handoff.block, query = handoff.query })
end

-- Only the live, leased block may forward a native shell request to its window.
local function poll_native_shells(state: any)
	if not state.token or not state.live or state.closing then return end
	local dir = state_dir()
	local ok, lease = pcall(tern.fs.read, dir .. "/managed-" .. state.cid .. ".lease", 4096)
	if not ok then return end
	local token, pane = lease:match("^(%S+)%s+(%d+)")
	if token ~= state.token or tonumber(pane) ~= state.pane then return end
	local listed, entries = pcall(tern.fs.list, dir)
	if not listed then return end
	state.native_shells = state.native_shells or {}
	local seen = {}
	local prefix = state.cid .. "-" .. state.token .. "-"
	for _, fname in ipairs(entries) do
		local id = fname:match("^native%-shell%-([%w-]+)%.json$")
		if not id or id:sub(1, #prefix) ~= prefix or not id:sub(#prefix + 1):match("^%d+$") then continue end
		local path = dir .. "/" .. fname
		local body = read_json(path)
		if not body or body.version ~= 1 or body.id ~= id or body.cid ~= state.cid
			or body.token ~= state.token or body.owner_pane ~= state.owner_pane or body.pane ~= state.pane then continue end
		seen[id] = true
		local pending = state.native_shells[id]
		if not pending then pending = { seen = tern.now() }; state.native_shells[id] = pending end
		local started_ok, started = pcall(tern.fs.read, dir .. "/native-shell-" .. id .. ".started", 256)
		if started_ok and started == id then pending.started = true end
		-- Missing window routes may recover during startup; the window's persistent
		-- request registry and helper's exclusive claim prevent duplicate execution.
		if not pending.started and tern.now() - pending.seen < 15000
			and (not pending.opened or tern.now() - pending.opened >= 500) then
			pending.opened = tern.now()
			pcall(function() route(state, path) end)
		end
	end
	for id in pairs(state.native_shells) do if not seen[id] then state.native_shells[id] = nil end end
end

local function clear_shell_handoffs(state: any)
	for id, handoff in pairs(state.shell_handoffs or {}) do
		if handoff.timer then handoff.timer:cancel() end
		pcall(tern.fs.remove, handoff.path)
		pcall(tern.fs.remove, handoff.path .. ".ack")
		state.shell_handoffs[id] = nil
	end
end

local request_seq = 0
local request_epoch = tostring(math.floor(tern.now() * 1000))
local pump

local function request(state: any, yz: any, body: any, done: any?)
	local cid = safe_cid(yz and yz.client_id)
	if body.op ~= "ping" and state.snapshot_error then
		notify(state, "error", "Yazi snapshot unavailable", state.snapshot_error)
		return
	end
	if state.closing or (body.op ~= "ping" and not refresh_live(state)) then
		notify(state, "error", "Yazi is offline", "Reconnect before using file actions")
		return
	end
	if state.cid and cid ~= state.cid then notify(state, "error", "Yazi client mismatch", nil); return end
	if not cid then notify(state, "error", "No Yazi client", nil); return end
	if body.args then
		local positional, options = {}, {}
		for i, value in ipairs(body.args) do positional[i] = value end
		for key, value in pairs(body.args) do
			if type(key) == "string" then options[key] = value end
		end
		body.args = { positional = positional, options = options }
	end
	request_seq += 1
	local prefix = state.token and (cid .. "-" .. state.token .. "-") or ""
	body.id = prefix .. request_epoch .. "-" .. tostring(state.pane) .. "-" .. tostring(request_seq)
	state.requests = state.requests or {}
	table.insert(state.requests, { cid = cid, body = body, done = done })
	pump(state)
end

pump = function(state: any)
	if state.sending or not state.requests or #state.requests == 0 then return end
	state.sending = true
	local job = state.requests[1]
	if state.closing or job.cid ~= state.cid or (job.body.op ~= "ping" and (state.snapshot_error or not refresh_live(state))) then
		state.sending = false
		state.requests = {}
		return
	end
	local body = job.body
	local path = state_dir() .. "/reply-" .. body.id .. ".json"
	local finished = false
	local function finish(reply: any)
		if finished then return end
		finished = true
		if state.closing or not session.current(state) then
			pcall(tern.fs.remove, path)
			state.sending = false
			return
		end
		if reply.ok and state.token and not refresh_live(state) then
			reply = { ok = false, error = "Yazi backend stopped; the last operation's outcome may be unknown" }
		end
		if reply.ok and not state.token then state.last_ack = tern.now(); state.live = true end
		state.sending = false
		table.remove(state.requests, 1)
		if not reply.ok then
			local was_live = state.live
			if not reply.rejected then
				state.last_ack = nil
				state.live = false
				state.requests = {}
				state.on_event({ kind = "reset_input" })
			end
			if body.op ~= "ping" or was_live then
				notify(state, "error", "Yazi request failed", reply.error or "No acknowledgement")
			end
		end
		if job.done then job.done(reply) end
		state.on_change()
		pump(state)
	end
	-- Plugin args have a second shell-style parse inside Yazi; quote the JSON
	-- as one positional argument, independently of the process argv boundary.
	local encoded = tern.json.encode(body)
	local quoted = "'" .. encoded:gsub("'", "'\\''") .. "'"
	local ok, err = pcall(function()
		tern.process.run({ "ya", "emit-to", job.cid, "plugin", "tern", quoted }, function(res)
			if not res or res.status ~= 0 then
				finish({ ok = false, error = res and res.stderr or "Missing send result" })
				return
			end
			local attempts = 0
			local poll
			poll = function()
				if finished then return end
				local read_ok, text = pcall(tern.fs.read, path, 256 * 1024)
				if read_ok then
					local decode_ok, reply = pcall(tern.json.decode, text)
					if decode_ok and reply.id == body.id then
						pcall(tern.fs.remove, path)
						finish(reply)
						return
					end
				end
				attempts += 1
				if attempts >= (body.op == "ping" and 60 or 200) then
					finish({ ok = false, error = "Timed out waiting for Yazi; outcome unknown, do not retry destructive actions blindly" })
				else tern.timer(50, poll) end
			end
			poll()
		end)
	end)
	if not ok then finish({ ok = false, error = tostring(err) }) end
end

local function open_shell(state: any, reply: any, query: string, block: boolean): boolean
	if state.closing or not session.current(state) then return false end
	local shell = reply.shell
	if not block or type(reply.id) ~= "string" or not reply.id:match("^[%w-]+$") or type(shell) ~= "table"
		or type(shell.command) ~= "string" or type(shell.cwd) ~= "string" or shell.block ~= true then
		notify(state, "error", "Invalid shell launch", "Yazi did not prepare a visible terminal command")
		return false
	end
	local path = state_dir() .. "/shell-" .. reply.id .. ".json"
	state.shell_handoffs = state.shell_handoffs or {}
	local handoff = { path = path, query = query, block = block,
		shell = { command = shell.command, cwd = shell.cwd, block = shell.block } }
	state.shell_handoffs[reply.id] = handoff
	local ok, err = pcall(function()
		tern.fs.write(path, tern.json.encode({ version = 1, id = reply.id, cid = state.cid,
			token = state.token, pane = state.pane, owner_pane = state.owner_pane, shell = shell }))
		route(state, path)
	end)
	if not ok then
		state.shell_handoffs[reply.id] = nil
		pcall(tern.fs.remove, path)
		notify(state, "error", "Cannot open shell terminal", tostring(err))
		return false
	end
	handoff.timer = tern.timer(30000, function()
		handoff.timer = nil
		if state.shell_handoffs[reply.id] ~= handoff then return end
		state.shell_handoffs[reply.id] = nil
		pcall(tern.fs.remove, path)
		pcall(tern.fs.remove, path .. ".ack")
		if state.closing or not session.current(state) then return end
		restore_command(state, handoff)
		notify(state, "error", "Cannot open shell terminal", handoff.nonce
			and "Timed out waiting for terminal launch acknowledgement; launch outcome unknown"
			or "Timed out preparing visible terminal; command was not launched")
		state.on_change()
	end)
	return true
end

local function shell_handoff_event(state: any, ev: any)
	local handoff = state.shell_handoffs and state.shell_handoffs[ev.request_id]
	if state.closing or not session.current(state) or not handoff or ev.cid ~= state.cid
		or ev.token ~= state.token or ev.pane ~= state.pane or type(ev.nonce) ~= "string"
		or not ev.nonce:match("^[%w-]+$") then return end
	if ev.act == "shell_handoff_check" then
		local shell = ev.shell
		if type(shell) ~= "table" or shell.command ~= handoff.shell.command
			or shell.cwd ~= handoff.shell.cwd or shell.block ~= handoff.shell.block then
			ev.error = "Shell launch payload changed; command was not launched"
		else
			if handoff.nonce == ev.nonce then return end
			handoff.nonce = ev.nonce
			local ok, err = pcall(function()
				tern.fs.write(handoff.path .. ".ack", tern.json.encode({ version = 1, id = ev.request_id,
					cid = state.cid, token = state.token, pane = state.pane, nonce = ev.nonce,
					shell = handoff.shell }))
				route(state, handoff.path)
			end)
			if ok then return end
			ev.error = tostring(err)
		end
	elseif ev.nonce ~= handoff.nonce then return end
	state.shell_handoffs[ev.request_id] = nil
	if handoff.timer then handoff.timer:cancel(); handoff.timer = nil end
	pcall(tern.fs.remove, handoff.path)
	pcall(tern.fs.remove, handoff.path .. ".ack")
	if ev.act ~= "shell_launched" then
		restore_command(state, handoff)
		notify(state, "error", "Cannot open shell terminal", ev.error or "Visible terminal launch failed")
	end
	state.on_change()
end

local function open_selected(state: any, selected: { string }?, ev: any?)
	if ev then
		local handoff = state.shell_handoffs and state.shell_handoffs[ev.request_id]
		local body = handoff and handoff.body
		if state.closing or not session.current(state) or not body or ev.cid ~= body.cid
			or ev.token ~= body.token or ev.pane ~= body.pane or ev.owner_pane ~= body.owner_pane
			or state.cid ~= body.cid or state.token ~= body.token or state.pane ~= body.pane
			or state.owner_pane ~= body.owner_pane or type(ev.nonce) ~= "string"
			or not ev.nonce:match("^[%w-]+$") then return end
		if ev.act ~= "files_handoff_check" and ev.nonce ~= handoff.nonce then return end
		local unchanged = type(ev.paths) == "table" and #ev.paths == #body.paths
		if unchanged then
			for i, path in ipairs(body.paths) do if ev.paths[i] ~= path then unchanged = false; break end end
		end
		local err = ev.error
		if not unchanged then err = "Selected-file paths changed during acknowledgement"
		elseif ev.act == "files_handoff_check" then
			if not refresh_live(state) then err = "The originating Yazi session is offline"
			elseif handoff.nonce == ev.nonce then return
			else
				handoff.nonce = ev.nonce
				local ok, result = pcall(function()
					tern.fs.write(handoff.path .. ".ack", tern.json.encode({ version = 1, id = body.id,
						cid = body.cid, token = body.token, pane = body.pane, owner_pane = body.owner_pane,
						nonce = ev.nonce, paths = body.paths }))
					route(state, handoff.path)
				end)
				if ok then return end
				err = tostring(result)
			end
		end
		state.shell_handoffs[ev.request_id] = nil
		pcall(tern.fs.remove, handoff.path)
		pcall(tern.fs.remove, handoff.path .. ".ack")
		if err or ev.act ~= "files_opened" then
			notify(state, "error", "Cannot open selected files", err or "Native file open failed")
		end
		return
	end
	if state.closing or not session.current(state) or not refresh_live(state) then
		notify(state, "error", "Yazi is offline", "Reconnect before using file actions")
		return
	end
	local cid = safe_cid(state.cid)
	if not cid then notify(state, "error", "No Yazi client", nil); return end
	-- Copy, never normalize or consult hover again: Alpha.txt and alpha.txt are
	-- distinct frozen targets even if the actor changes selection before ACK.
	local paths = {}
	for i, path in ipairs(selected or {}) do
		if type(path) ~= "string" or path:sub(1, 1) ~= "/" or path:find("%z") then
			notify(state, "error", "Cannot open selected files", "Yazi supplied an invalid selected path")
			return
		end
		paths[i] = path
	end
	if #paths == 0 then return end
	request_seq += 1
	local prefix = state.token and (cid .. "-" .. state.token .. "-") or ""
	local id = prefix .. request_epoch .. "-" .. tostring(state.pane) .. "-" .. tostring(request_seq)
	local path = state_dir() .. "/files-" .. id .. ".json"
	local body = { version = 1, id = id, cid = cid, token = state.token,
		pane = state.pane, owner_pane = state.owner_pane, paths = paths }
	-- Reuse the owned shell inbox lifecycle so close/reload removes both files.
	state.shell_handoffs = state.shell_handoffs or {}
	state.shell_handoffs[id] = { path = path, body = body }
	local ok, err = pcall(function()
		tern.fs.write(path, tern.json.encode(body))
		route(state, path)
	end)
	if not ok then
		state.shell_handoffs[id] = nil
		pcall(tern.fs.remove, path)
		pcall(tern.fs.remove, path .. ".ack")
		notify(state, "error", "Cannot open selected files", tostring(err))
	end
end

local function action(state: any, yz: any, name: string, args: any)
	request(state, yz, { op = "action", action = name, args = args })
end


local function terminal_here(state: any, val: string)
	local ok, err = pcall(function()
		tern.process.run({ "tern", "split", tostring(state.pane), "down", "--cwd", val }, function(res)
			if state.closing or not session.current(state) then return end
			if not res or res.status ~= 0 then
		notify(state, "error", "Cannot open terminal", res and res.stderr or "Missing launch result")
			end
		end)
	end)
	if not ok then notify(state, "error", "Cannot open terminal", tostring(err)) end
end

local function pending_filter(state: any): boolean
	local requests = state.requests
	if not requests then return false end
	for _, job in ipairs(requests) do
		if job.body.op == "filter" then return true end
	end
	return false
end

M.request = request
M.action = action
M.pending_filter = pending_filter
M.poll_native_shells = poll_native_shells
M.clear_handoffs = clear_shell_handoffs
M.open_shell = open_shell
M.shell_handoff_event = shell_handoff_event
M.open_selected = open_selected
M.terminal_here = terminal_here
return M
