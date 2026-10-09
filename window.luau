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
		if not record.stopped and record.pane and cx.session:pane(record.pane)
			and (record.pane == focused or record.owner_pane == focused) then
			pane = record.pane
			break
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
	record.pane = cx:new_block(BLOCK_KIND,
		{ record.cid, record.token, tostring(record.owner_pane), record.cwd, record.nonce }, "beside", { focus = true })
	record.started = tern.now()
	record.adopting = false
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
			if record.pane == ev.pane then managed[cid] = nil end
		end
	end
end)

local function arm_timer()
	tern.timer(500, function(cx)
		if cx then
			local ok, err = pcall(poke, cx)
			if not ok then tern.log.warn("Yazi managed poll failed", tostring(err)) end
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
