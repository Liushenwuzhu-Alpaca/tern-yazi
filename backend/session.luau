-- Host-side snapshot, lease, discovery and lifecycle ownership.
local path = require('../shared/path')
local inbox = require('./inbox')
local safe_cid = path.safe_cid
local state_dir, read_json = inbox.state_dir, inbox.read_json
local model = require('./model')
type YaziState = model.YaziState
local STATE_BYTES, LISTING_BYTES = 256 * 1024, 16 * 1024 * 1024
local running, launches = {}, {}
local M = {}
local function standalone_binding(state)
	local binding = read_json(state_dir() .. "/native-" .. tostring(state.pane) .. ".json")
	if binding and binding.owner_kind == "standalone" and binding.native_pane == state.pane
		and binding.owner_pane == state.pane and safe_cid(binding.client_id)
		and type(binding.token) == "string" and binding.token:match("^[%w-]+$") then return binding end
	return nil
end
local function adopt_standalone(state)
	if state.owner_kind ~= "standalone" then return end
	local binding = standalone_binding(state)
	if not binding or (state.token and state.token ~= binding.token) then return end
	state.cid, state.token, state.owner_pane = binding.client_id, binding.token, state.pane
	state.source_cwd = binding.source_cwd or binding.cwd or state.source_cwd
	state.lease_nonce = state.lease_nonce or tern.fs.read("/proc/sys/kernel/random/uuid", 64):match("^[%w-]+")
end

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

-- Exact bytes avoid redundant decoding. Bounds reject the entire input; they
-- never clip filenames, selection paths, or the order owned by Yazi.
local function cached_json(state: any, field: string, filename: string, max_bytes: number): (any?, string?)
	local ok, text = pcall(tern.fs.read, filename, max_bytes)
	if not ok then return nil, tostring(text):gsub("[%z\1-\31\127]", " "):sub(1, 240) end
	local cached = state[field]
	if cached and cached.filename == filename and cached.text == text then return cached.value, cached.error end
	local decoded, value = pcall(tern.json.decode, text)
	local err = not decoded and tostring(value):gsub("[%z\1-\31\127]", " "):sub(1, 240) or nil
	if decoded and type(value) ~= "table" then err = "Expected a snapshot object" end
	value = decoded and type(value) == "table" and value or nil
	state[field] = { filename = filename, text = text, value = value, error = err }
	return value, err
end

local function with_icons(folder: any, overrides: any): any?
	if not folder then return nil end
	return { cwd = folder.cwd, files = folder.files, file_dirs = folder.file_dirs,
		file_icons = folder.file_icons, file_count = folder.file_count, icon_overrides = overrides }
end

local function refresh_live(state: any): boolean
	local live = false
	if not state.closing and state.token and state.cid then
		local health = cached_json(state, "health_json", state_dir() .. "/managed-" .. state.cid .. ".json", 256 * 1024)
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
	state.snapshot_error = nil
	state.snapshot_waiting = nil
	if state.snapshot_cid ~= state.cid or state.snapshot_token ~= state.token then
		state.snapshot_cid, state.snapshot_token = state.cid, state.token
		state.snapshot, state.snapshot_listing = nil, nil
		state.dynamic_json, state.listing_json = nil, nil
		state.snapshot_revision = (state.snapshot_revision or 0) + 1
	end
	if not state.cid then return nil end
	local filename = sdir .. "/state-" .. state.cid .. ".json"
	local parsed, err = cached_json(state, "dynamic_json", filename, STATE_BYTES)
	if not parsed then
		if state.snapshot or tern.fs.exists(filename) then
			state.snapshot_error = "Cannot read complete Yazi state (256 KiB maximum): " .. (err or "Unavailable snapshot")
		end
		return nil
	end
	if parsed == state.snapshot then return parsed end
	if tostring(parsed.client_id) ~= state.cid or type(parsed.listing_epoch) ~= "string"
		or type(parsed.listing_revision) ~= "number" then
		state.snapshot_error = "Yazi snapshot identity or listing generation is invalid"
		return nil
	end
	local listing = state.snapshot_listing
	if not listing or listing.epoch ~= parsed.listing_epoch or listing.revision ~= parsed.listing_revision then
		local listing_error
		listing, listing_error = cached_json(state, "listing_json", sdir .. "/listing-" .. state.cid .. ".json", LISTING_BYTES)
		if not listing then
			state.snapshot_error = "Cannot read complete Yazi listing (16 MiB maximum): " .. (listing_error or "Unavailable listing")
			return nil
		end
		-- The actor replaces listing before dynamic state. Never combine generations.
		if not listing or listing.epoch ~= parsed.listing_epoch or listing.revision ~= parsed.listing_revision then
			state.snapshot_waiting = true
			return state.snapshot
		end
		listing = model.reuse_listing(listing, state.snapshot_listing)
		state.snapshot_listing = listing
	end
	parsed.cwd, parsed.files = listing.cwd, listing.files
	parsed.file_dirs, parsed.file_icons, parsed.file_count = listing.file_dirs, listing.file_icons, listing.file_count
	parsed.selected_urls, parsed.marked_urls = listing.selected_urls, listing.marked_urls
	parsed.parent = with_icons(listing.parent, parsed.parent_icon_overrides)
	parsed.preview = with_icons(listing.preview, parsed.preview_icon_overrides)
	model.reuse_dynamic(parsed, state.snapshot)
	if not model.same_snapshot(parsed, state.snapshot) then
		state.snapshot_revision = (state.snapshot_revision or 0) + 1
	end
	state.snapshot = parsed
	return parsed
end

local function latest_state(state: any): YaziState?
	return not state.snapshot_error and state.snapshot or nil
end

local function close_route(state): string?
	if state.owner_kind == "standalone" or not state.token or not state.cid or not state.owner_pane then return nil end
	return state_dir() .. "/return-" .. state.cid .. "-" .. state.token .. "." .. tostring(state.pane)
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
		if state.owner_kind == "standalone" then
			local binding = standalone_binding(state)
			if binding and binding.token == state.token then
				pcall(tern.fs.write, stem .. ".stop", state.token .. "\n")
			end
		end
	end
end
local function stop_owned(state)
	adopt_standalone(state)
	stop_backend(state)
	if state.owner_kind == "standalone" and not state.token and state.startup_attempted then
		local deadline = tern.now() + 15000
		local stop_pending
		stop_pending = function()
			adopt_standalone(state)
			if state.token then stop_backend(state)
			elseif tern.now() < deadline then tern.timer(50, stop_pending) end
		end
		stop_pending()
	end
	if state.owner_kind == "standalone" then
		local store = tern.kv.get("native_launches") or {}
		store[tostring(state.pane)] = nil
		tern.kv.set("native_launches", store)
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
	stop_owned(state)
end
local function standalone_executables(): (string, string)
	local helper = tern.kv.get("launcher_path") or tern.getenv("TERN_YAZI_LAUNCHER_PATH")
	if not helper then
		-- The installer exposes prefix/bin/yazi, not libexec, on PATH. Resolve
		-- the paired helper and original-path data using that same prefix.
		for directory in (tern.getenv("PATH") or ""):gmatch("[^:]+") do
			if directory:sub(1, 1) ~= "/" then continue end
			local candidate = directory .. "/../libexec/tern-yazi-launch"
			if tern.fs.exists(candidate) and tern.fs.exists(candidate .. ".real") then helper = candidate; break end
		end
		helper = helper or (tern.getenv("HOME") or "") .. "/.local/libexec/tern-yazi-launch"
	end
	local real = tern.kv.get("real_yazi") or tern.getenv("TERN_YAZI_REAL")
	if not real then
		local ok, text = pcall(tern.fs.read, helper .. ".real", 4096)
		if ok then real = text:match("^([^\r\n]+)") end
	end
	return helper, real or "/usr/bin/yazi"
end
local function start_standalone(state)
	if state.owner_kind ~= "standalone" then return end
	adopt_standalone(state)
	if state.token then return end
	local store = tern.kv.get("native_launches") or {}
	local entry = store[tostring(state.pane)]
	if state.startup_attempted or entry then
		state.startup_attempted = true
		return
	end
	state.startup_attempted = true
	store[tostring(state.pane)] = { cwd = state.source_cwd }
	tern.kv.set("native_launches", store)
	local helper, real = standalone_executables()
	local ok, err = pcall(function()
		tern.process.run({ helper, "--standalone-pane", tostring(state.pane), "--real", real, "--" },
			{ cwd = state.source_cwd }, function(result)
				local current_state = running[state.pane]
				if not current_state or current_state.closing or current_state.owner_kind ~= "standalone" then return end
				if result and result.status == 0 then return end
				local detail = result and result.stderr or "Supervisor exited without a result"
				detail = tostring(detail):gsub("[%z\1-\31\127]", " "):sub(1, 240)
				current_state.startup_error = detail
				current_state.on_event({ kind = "toast", level = "error", title = "Cannot start Yazi", detail = detail })
				current_state.on_change()
			end)
	end)
	if not ok then
		state.startup_error = tostring(err):sub(1, 240)
		state.on_event({ kind = "toast", level = "error", title = "Cannot start Yazi", detail = state.startup_error })
	end
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
		adopt_standalone(state)
		local pane_alive, owner_alive = false, state.owner_pane == nil
		for _, info in ipairs(state.pane_list()) do
			if info.pane == state.pane then pane_alive = true end
			if info.pane == state.owner_pane then owner_alive = true end
		end
		if not pane_alive or not owner_alive then
			state.closing = true
			if state.on_close then state.on_close() end
			running[state.pane] = nil
			stop_owned(state)
			return
		end
		local yz = read_latest_state(state)
		if state.on_poll then state.on_poll(yz) end
		local revision = state.snapshot_revision
		local config_stamp = state.preview_config()
		if revision ~= state.render_revision or state.live ~= state.render_live or config_stamp ~= state.render_config
			or state.snapshot_error ~= state.render_error then
			state.render_revision, state.render_live, state.render_config = revision, state.live, config_stamp
			state.render_error = state.snapshot_error
			state.on_change(true)
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
	local owner_kind = args[3] and "shell" or saved.owner_kind
	if not owner_kind then owner_kind = args[1] and "attach" or "standalone" end
	return {
		pane = pane, cid = cid, token = token, lease_nonce = nonce,
		owner_kind = owner_kind, owner_pane = tonumber(args[3]) or saved.owner_pane,
		source_cwd = args[4] or saved.source_cwd, live = false,
		startup_attempted = saved.startup_attempted, startup_error = saved.startup_error,
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
	start_standalone(state)
	read_latest_state(state)
	poll_block(state)
end

local function save(state: any): any
	return { cid = state.cid, token = state.token, owner_pane = state.owner_pane,
		owner_kind = state.owner_kind, startup_attempted = state.startup_attempted,
		startup_error = state.startup_error, source_cwd = state.source_cwd, lease_nonce = state.lease_nonce }
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
M.close_route = close_route
M.read_latest_state = read_latest_state
M.latest_state = latest_state
M.refresh_live = refresh_live
M.managed_poll = managed_poll
M.start_discovery = start_discovery
M.save = save
return M
