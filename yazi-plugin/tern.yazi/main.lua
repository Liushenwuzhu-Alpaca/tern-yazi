-- tern.yazi: exports yazi's state to the tern-yazi companion over DDS, and
-- receives companion commands. Zero patches — plugin API + DDS only.
--
-- Install: link this directory into ~/.config/yazi/plugins/, then add
--   require("tern"):setup()
-- to ~/.config/yazi/init.lua.

local M = {}

-- Custom kinds (alphanumeric + dashes; must not collide with builtin kinds).
local KIND_HOVER = "tern-hover"
local KIND_CD = "tern-cd"
local KIND_STATE = "tern-state"
local KIND_CMD = "tern-cmd"
local KIND_ACK = "tern-ack"

-- Broadcast (receiver 0) to every remote subscriber, e.g. `ya sub <kind>`.
-- ps.* is sync-context only; every call site below runs in one.
local function broadcast(kind, value)
	ps.pub_to(0, kind, value)
end

-- ps.sub callbacks run in the sync context after the manager settles
-- (yazi publishes hover/cd post-update), so read cx directly here.
local function selected_count()
	return #cx.active.selected
end

local function read_hovered()
	local h = cx.active.current.hovered
	if not h then
		return { url = nil, selected = selected_count() }
	end
	local cha = h.cha
	local mtime = cha and tonumber(cha.mtime)
	return {
		url = tostring(h.url),
		dir = cha and cha.is_dir or false,
		size = cha and cha.len or nil,
		mtime = mtime and math.floor(mtime) or nil,
		selected = selected_count(),
	}
end

local function read_folder()
	local cur = cx.active.current
	local names = {}
	for i = 1, #cur.files do
		local url = cur.files[i].url
		names[i] = url.name or tostring(url)
	end
	return { url = tostring(cur.cwd), files = names, selected = selected_count() }
end

-- yazi fires hover on every redraw, not just on moves; dedupe at the source.
local last_hover = nil
local function publish_hover()
	local payload = read_hovered()
	if payload.url ~= last_hover then
		last_hover = payload.url
		broadcast(KIND_HOVER, payload)
	end
end

local function publish_folder()
	last_hover = nil
	broadcast(KIND_CD, read_folder())
end

-- Selection changes without hover moves (select-all) and task progress have no
-- builtin events of their own, but every redraw refires hover/cd callbacks —
-- piggyback a deduped pulse onto them instead of polling from async context.
-- Aggregate task state: totals come from cx.tasks.summary (status-bar
-- component source), byte counters summed over cx.tasks.snaps. All access
-- stays inside pulse()'s pcall so a shape change degrades to an error pulse.
local function read_pulse()
	local summary = cx.tasks.summary
	local found, processed = 0, 0
	for _, snap in ipairs(cx.tasks.snaps) do
		local p = snap.prog
		if p and p.total_bytes then
			found = found + p.total_bytes
			processed = processed + (p.processed_bytes or 0)
		end
	end
	return {
		selected = selected_count(),
		tasks = {
			total = summary.total,
			succ = summary.success,
			fail = summary.failed,
			found = found,
			processed = processed,
		},
	}
end

local function pulse_key(pulse_value)
	local t = pulse_value.tasks
	return table.concat({ pulse_value.selected, t.total, t.succ, t.fail, t.found, t.processed }, "|")
end

local last_pulse = nil
local function pulse()
	local ok, p = pcall(read_pulse)
	if not ok then
		if not last_pulse then
			last_pulse = "error"
			broadcast(KIND_STATE, { error = tostring(p) })
		end
		return
	end
	local key = pulse_key(p)
	if key ~= last_pulse then
		last_pulse = key
		broadcast(KIND_STATE, p)
	end
end

-- State inbox writer for native Tern companion plugin (Option A).
local state_seq = 0
local last_dump_key = nil

local function state_dir()
	local base = os.getenv("XDG_RUNTIME_DIR")
	if not base or base == "" then
		base = "/tmp"
	end
	return base .. "/tern-yazi"
end

local function get_client_id()
	local env_id = os.getenv("YAZI_ID")
	if env_id and env_id ~= "" then
		return env_id
	end
	return "default"
end

local function dump_state()
	local active = cx.active
	if not active or not active.current then
		return
	end

	local folder = read_folder()
	local hovered = read_hovered()
	local ok_p, pulse_data = pcall(read_pulse)
	local tasks_data = ok_p and pulse_data.tasks or { total = 0, succ = 0, fail = 0, found = 0, processed = 0 }
	local selected = ok_p and pulse_data.selected or selected_count()

	local key = table.concat({
		folder.url,
		#folder.files,
		hovered.url or "",
		selected,
		tasks_data.total,
		tasks_data.succ,
		tasks_data.fail,
		tasks_data.found,
		tasks_data.processed,
	}, ";")

	if key == last_dump_key then
		return
	end
	last_dump_key = key
	state_seq = state_seq + 1

	local sdir = state_dir()
	os.execute("mkdir -p " .. sdir)

	local cid = get_client_id()
	local payload = {
		seq = state_seq,
		client_id = tonumber(cid) or cid,
		cwd = folder.url,
		files = folder.files,
		selected = selected,
		hovered = hovered.url and hovered or nil,
		tasks = tasks_data,
	}

	local ok_enc, encoded = pcall(ya.json_encode, payload)
	if not ok_enc or not encoded then
		return
	end

	local tmp_file = string.format("%s/state-%s.tmp", sdir, cid)
	local dest_file = string.format("%s/state-%s.json", sdir, cid)
	local f = io.open(tmp_file, "w")
	if f then
		f:write(encoded)
		f:close()
		os.rename(tmp_file, dest_file)
	end
end

function M:setup()
	-- Builtin local event bodies carry only { tab }; snapshot `cx` instead.
	ps.sub("hover", function()
		publish_hover()
		pulse()
		dump_state()
	end)
	ps.sub("cd", function()
		publish_folder()
		pulse()
		dump_state()
	end)

	-- Companion command channel.
	--   {"op":"hello"} -> full snapshot (companion startup / reattach)
	--   anything else  -> ack echo (round-trip probe)
	ps.sub_remote(KIND_CMD, function(body)
		if type(body) == "table" and body.op == "hello" then
			publish_folder()
			publish_hover()
		else
			broadcast(KIND_ACK, { got = body })
		end
		pulse()
	end)
	-- No pubsub event fires for task progress on 26.9, but status children
	-- redraw together with the built-in progress gauge: pulse from there.
	Status:children_add(function()
		pulse()
		dump_state()
		return ui.Line("")
	end, 1000, Status.RIGHT)
end

return M
