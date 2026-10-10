-- Host-side snapshot, lease, discovery and lifecycle ownership.
local path = require('../shared/path')
local inbox = require('./inbox')
local safe_cid = path.safe_cid
local state_dir, read_json = inbox.state_dir, inbox.read_json
local model = require('./model')
type YaziState = model.YaziState
local running, launches = {}, {}
local M = {}

local function discover_launches(can_open: any, on_open: any)
	local dir = state_dir()
	if not tern.fs.exists(dir) then return end
	local seen = {}
	for _, fname in ipairs(tern.fs.list(dir)) do
		local cid = fname:match("^managed%-(%d+)%.json$")
		if not cid then continue end
		local health = read_json(dir .. "/" .. fname)
		if not health or health.client_id ~= cid or type(health.token) ~= "string"
			or not health.token:match("^[%w-]+$") or type(health.owner_pane) ~= "number"
			or type(health.seq) ~= "number" or type(health.cwd) ~= "string" then continue end
		seen[cid] = true
		local record = launches[cid]
		if not record or record.token ~= health.token then
			record = { token = health.token, seq = health.seq }
			launches[cid] = record
		elseif not record.opened and record.seq ~= health.seq then
			if can_open(health.owner_pane) then
				-- A progressing supervisor, not an old snapshot, authorizes startup.
				on_open(health.owner_pane, dir .. "/" .. fname)
				record.opened = true
			end
		end
		record.seq = health.seq
	end
	for cid in pairs(launches) do if not seen[cid] then launches[cid] = nil end end
end

local function refresh_live(state: any): boolean
	local live = false
	if not state.closing and state.token and state.cid then
		local health = read_json(state_dir() .. "/managed-" .. state.cid .. ".json")
		if health and health.client_id == state.cid and health.token == state.token
			and type(health.seq) == "number" and health.owner_pane == state.owner_pane then
			if health.seq ~= state.health_seq then
				if state.health_seq ~= nil then state.health_seen = tern.now() end
				state.health_seq = health.seq
			end
			live = state.health_seen ~= nil and tern.now() - state.health_seen < 3000
		end
	elseif not state.closing then
		-- Idle snapshots have no expiry. Only an actor acknowledgement proves life.
		live = state.last_ack ~= nil and tern.now() - state.last_ack < 3000
	end
	state.live = live
	if not live then
		state.on_event({ kind = "reset_input", clear_trash = true })
	end
	return live
end

local function read_latest_state(state: any): YaziState?
	local sdir = state_dir()
	refresh_live(state)
	if state.cid then
		local parsed = read_json(sdir .. "/state-" .. state.cid .. ".json")
		if parsed then parsed.client_id = state.cid end
		return parsed
	end
	if not tern.fs.exists(sdir) then return nil end
	local ok, entries = pcall(tern.fs.list, sdir)
	if not ok then return nil end
	local newest, score = nil, -1
	for _, fname in ipairs(entries) do
		local cid = fname:match("^state%-(%d+)%.json$")
		if cid and not tern.fs.exists(sdir .. "/managed-" .. cid .. ".json") then
			local parsed = read_json(sdir .. "/" .. fname)
			if parsed then
				local candidate = tonumber(parsed.ts) or tonumber(parsed.seq) or 0
				if candidate > score then
					parsed.client_id = cid
					newest, score = parsed, candidate
				end
			end
		end
	end
	if newest then state.cid = newest.client_id end
	return newest
end

local function stop_backend(state: any)
	if state.cid and state.token then
		local stem = state_dir() .. "/managed-" .. state.cid
		local health = read_json(stem .. ".json")
		if health and health.client_id == state.cid and health.token == state.token then
			pcall(tern.fs.write, stem .. ".stop", state.token .. "\n")
			local ok, lease = pcall(tern.fs.read, stem .. ".lease", 4096)
			if ok and lease:match("^(%S+)") == state.token then pcall(tern.fs.write, stem .. ".lease", "") end
		end
	end
end

local function current(state: any): boolean
	return running[state.pane] == state
end

local function close(state: any)
	state.closing = true
	state.live = false
	state.requests = {}
	if state.on_close then state.on_close() end
	if state.timer then state.timer:cancel(); state.timer = nil end
	running[state.pane] = nil
	stop_backend(state)
end

local function managed_poll(state: any, ev: any, pane: any)
	-- PaneInfo has no launch args. This token-bound nonce ACK proves the hinted
	-- pane is this client, even after a reload or pane-id reuse.
	if ev.cid ~= state.cid or ev.token ~= state.token or type(ev.nonce) ~= "string"
		or not ev.nonce:match("^[%w-]+$") or not refresh_live(state) then return end
	state.lease_nonce = ev.nonce
	local lease = string.format("%s %d %d %d %s\n", state.token, pane.pane,
		math.max(1, pane.rows), math.max(1, pane.cols), ev.nonce)
	pcall(tern.fs.write, state_dir() .. "/managed-" .. state.cid .. ".lease", lease)
end

-- Host timers keep idle daemon-owned panes alive independently of window frames.
local function poll_block(state)
	state.timer = tern.timer(500, function()
		state.timer = nil
		if state.closing or not current(state) then return end
		local pane_alive, owner_alive = false, state.owner_pane == nil
		for _, info in ipairs(state.pane_list()) do
			if info.pane == state.pane then pane_alive = true end
			if info.pane == state.owner_pane then owner_alive = true end
		end
		if not pane_alive or not owner_alive then
			state.closing = true
			if state.on_close then state.on_close() end
			running[state.pane] = nil
			stop_backend(state)
			return
		end
		local yz = read_latest_state(state)
		if state.on_poll then state.on_poll(yz) end
		local seq = yz and (yz.ts or yz.seq)
		local config_stamp = state.preview_config()
		if seq ~= state.render_seq or state.live ~= state.render_live or config_stamp ~= state.render_config then
			state.render_seq, state.render_live, state.render_config = seq, state.live, config_stamp
			state.on_change()
		end
		poll_block(state)
	end)
end

local function new(args: any, saved: any, pane: number): any
	args = args or {}
	saved = type(saved) == "table" and saved or {}
	local cid = safe_cid(args[1] or saved.cid)
	if cid and not cid:match("^%d+$") then cid = nil end
	local token = args[2] or saved.token
	if type(token) ~= "string" or not token:match("^[%w-]+$") then token = nil end
	local nonce = args[5] or saved.lease_nonce
	if type(nonce) ~= "string" or not nonce:match("^[%w-]+$") then nonce = nil end
	return {
		pane = pane, cid = cid, token = token, lease_nonce = nonce,
		owner_pane = tonumber(args[3]) or saved.owner_pane,
		source_cwd = args[4] or saved.source_cwd, live = false,
	}
end

local function install(state: any)
	local previous = running[state.pane]
	if previous and previous.timer then previous.timer:cancel() end
	if previous then
		previous.closing = true
		if previous.on_close then previous.on_close() end
	end
	running[state.pane] = state
	poll_block(state)
end

local function save(state: any): any
	return { cid = state.cid, token = state.token, owner_pane = state.owner_pane,
		source_cwd = state.source_cwd, lease_nonce = state.lease_nonce }
end

local function start_discovery(can_open: any, on_open: any)
	local poll
	poll = function()
		local ok, err = pcall(discover_launches, can_open, on_open)
		if not ok then tern.log.warn("Yazi startup discovery failed", tostring(err)) end
		tern.timer(500, poll)
	end
	tern.timer(500, poll)
end

M.new = new
M.install = install
M.current = current
M.close = close
M.read_latest_state = read_latest_state
M.refresh_live = refresh_live
M.managed_poll = managed_poll
M.start_discovery = start_discovery
M.save = save
return M
