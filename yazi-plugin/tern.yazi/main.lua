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

local function selected_urls()
	local urls = {}
	for _, file in pairs(cx.active.selected) do
		urls[#urls + 1] = tostring(file.url)
	end
	table.sort(urls)
	return urls
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
local function read_parent()
	local p = cx.active.parent
	if not p or not p.cwd then
		return nil
	end
	local names = {}
	for i = 1, math.min(30, #(p.files or {})) do
		local url = p.files[i].url
		names[i] = url.name or tostring(url):match("([^/]+)$") or tostring(url)
	end
	return {
		cwd = tostring(p.cwd),
		files = names,
	}
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
	local selected_paths = selected_urls()
	local marked = {}
	for i = 1, #active.current.files do
		local file = active.current.files[i]
		if file:is_marked() ~= 0 then marked[#marked + 1] = tostring(file.url) end
	end
	local mode = active.mode.is_normal and "normal" or (active.mode.is_select and "select" or "unset")

	local key = table.concat({
		folder.url,
		ya.json_encode(folder.files),
		hovered.url or "",
		selected,
		tasks_data.total,
		tasks_data.succ,
		tasks_data.fail,
		tasks_data.found,
		tasks_data.processed,
		ya.json_encode(selected_paths),
		mode,
		ya.json_encode(marked),
	}, ";")

	if key == last_dump_key then
		return
	end
	last_dump_key = key
	state_seq = state_seq + 1
	local sdir = state_dir()
	local cid = tostring(get_client_id())
	local cur_time = (ya.time and ya.time()) or os.time()
	local payload = {
		ts = cur_time,
		seq = state_seq,
		client_id = cid,
		parent = read_parent(),
		cwd = folder.url,
		files = folder.files,
		selected = selected,
		selected_urls = selected_paths,
		mode = mode,
		marked_urls = marked,
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

-- A DDS send is not an actor acknowledgement. Every request has its own reply.
local function reply(req, value)
	value.id = req.id
	local path = state_dir() .. "/reply-" .. req.id .. ".json"
	local f = assert(io.open(path .. ".tmp", "w"))
	f:write(assert(ya.json_encode(value)))
	f:close()
	assert(os.rename(path .. ".tmp", path))
end

local snapshot = ya.sync(function()
	last_dump_key = nil
	dump_state()
end)

local targets = ya.sync(function(_, target)
	local paths = {}
	for _, file in pairs(cx.active.selected) do paths[tostring(file.url)] = true end
	for i = 1, #cx.active.current.files do
		local f = cx.active.current.files[i]
		local marked = f:is_marked()
		if marked == 1 then paths[tostring(f.url)] = true
		elseif marked == 2 then paths[tostring(f.url)] = nil end
	end
	local result = {}
	for path in pairs(paths) do result[#result + 1] = path end
	if #result == 0 and target then result[1] = target end
	table.sort(result)
	return result
end)

-- Sync emits preempt the normal queue. The FIFO guard runs after the toggles
-- and before the actor; an unrelated reveal cannot change the approved set.
local commit = ya.sync(function(self, req, files)
	assert(not self.pending, "Another target operation is pending")
	self.pending = req
	ya.emit("escape", { visual = true, select = true })
	for _, file in ipairs(files) do ya.emit("toggle", { file, state = "on" }) end
	ya.emit("plugin", { "tern", "finalize", mode = "sync" })
end)

-- Only actor names present in the 26.9.1 manager executor are accepted.
local command_actors = {}
for name in ("cd arrow leave enter back forward reveal follow stash open yank unyank toggle toggle_all visual_arrow visual_mode escape copy shell hidden linemode filter filter_do sort refresh quit close suspend seek"):gmatch("%S+") do
	command_actors[name] = true
end

function M:entry(job)
	local stage = job.args[1]
	if stage == "finalize" then
		local req = assert(self.pending, "Missing pending target request")
		local actual = selected_urls()
		local expected = req.paths
		local matches = cx.active.mode.is_normal and #actual == #expected
		for i, path in ipairs(expected) do matches = matches and actual[i] == path end
		if not matches then
			self.pending = nil
			reply(req, { ok = false, error = "Target selection changed; operation refused" })
			return
		end
		if req.op == "trash_commit" then
			ya.emit("remove", { force = true }) -- Trash only; Tern confirmed these paths.
		else
			ya.emit("yank", {})
		end
		ya.emit("plugin", { "tern", "settled", mode = "sync" })
		return
	elseif stage == "settled" then
		local req = assert(self.pending)
		self.pending = nil
		last_dump_key = nil
		dump_state()
		reply(req, { ok = true, paths = req.paths, queued = req.op == "trash_commit" })
		return
	end

	local req = assert(ya.json_decode(stage))
	assert(type(req.id) == "string" and req.id:match("^[%w-]+$"), "Invalid request id")
	local ok, err = pcall(function()
		if req.args then
			local args = {}
			for i, value in ipairs(req.args.positional or {}) do args[i] = value end
			for key, value in pairs(req.args.options or {}) do args[key] = value end
			req.args = args
		end
		if req.op == "command" then
			assert(command_actors[req.action] or req.action == "remove", "Unknown or unsupported manager command: " .. tostring(req.action))
			assert(not req.args.interactive, "Local command input does not open another interactive Yazi prompt")
			if req.action == "remove" then
				assert(not req.args.permanently and not req.args.force, "Use plain d for confirmed trash; force/permanent removal is disabled")
				reply(req, { ok = true, paths = targets(req.target), confirm_trash = true })
				return
			elseif req.action == "yank" then
				assert(not req.args.cut, "Cut is not a companion shortcut; x remains extraction")
				req.op = "yank"
			elseif req.action == "toggle" then req.op = "toggle"
			elseif req.action == "open" and #req.args == 0 then req.op = "open"
			elseif req.action == "filter" or req.action == "filter_do" then
				req.op = "filter"
				req.query = req.args[1] or ""
			else
				ya.exec(req.action, req.args)
				snapshot()
				reply(req, { ok = true, queued = true })
				return
			end
		end
		if req.op == "trash_prepare" then
			reply(req, { ok = true, paths = targets(req.target) })
		elseif req.op == "trash_commit" or req.op == "yank" then
			if req.op == "yank" then req.paths = targets(req.target) end
			assert(type(req.paths) == "table" and #req.paths > 0, "No targets")
			table.sort(req.paths)
			local files = {}
			for _, path in ipairs(req.paths) do
				local file, e = fs.file(Url(path))
				assert(file, tostring(e))
				files[#files + 1] = file
			end
			commit(req, files)
		elseif req.op == "toggle" then
			local file, e = fs.file(Url(assert(req.target)))
			assert(file, tostring(e))
			ya.exec("toggle", { file })
			snapshot()
			reply(req, { ok = true })
		elseif req.op == "open" then
			ya.exec("open", { Url(assert(req.target)), cwd = Url(req.cwd) })
			reply(req, { ok = true, queued = true })
		elseif req.op == "filter" then
			ya.exec("filter_do", { req.query or "", insensitive = true, done = true })
			snapshot()
			reply(req, { ok = true })
		elseif req.op == "action" then
			assert(req.action == "arrow" or req.action == "cd" or req.action == "reveal" or req.action == "visual_mode" or req.action == "escape" or req.action == "hidden", "Unsupported companion action")
			ya.exec(req.action, req.args or {})
			snapshot()
			reply(req, { ok = true, queued = true })
		else
			error("Unknown companion operation")
		end
	end)
	if not ok then reply(req, { ok = false, error = tostring(err) }) end
end

function M:setup()
	pcall(os.execute, "mkdir -p " .. state_dir())
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
