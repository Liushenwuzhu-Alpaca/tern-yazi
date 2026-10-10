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

-- One hover-owned calculation only; never accumulate a path-indexed cache.
local directory_usage = { token = 0 }
local directory_size

local function read_hovered()
	local h = cx.active.current.hovered
	if not h then
		pcall(directory_size, nil)
		return { url = nil, selected = selected_count() }
	end
	local cha = h.cha
	local mtime = cha and tonumber(cha.mtime)
	-- Public Cha:perm() returns Unix permission text, or nil when unavailable.
	local ok_perm, permissions = pcall(function() return cha and not cha.is_dummy and cha:perm() or nil end)
	-- Cha.len on a directory is its inode, not the size of its contents.
	local ok_size, usage = pcall(directory_size, h)
	local size
	if cha and not cha.is_dummy then
		if cha.is_dir then size = ok_size and usage or nil
		else size = cha.len end
	end
	return {
		url = tostring(h.url),
		dir = cha and cha.is_dir or false,
		size = size,
		mtime = mtime and math.floor(mtime) or nil,
		permissions = ok_perm and type(permissions) == "string" and permissions ~= "" and permissions or nil,
		selected = selected_count(),
	}
end

local icon_error_reported = false
local function read_icon_text(file, base)
	local hovered = file.is_hovered
	if base then hovered = false end
	local icon = th.icon:match(file, { hovered = hovered })
	return icon and icon.text or false
end

-- Reuse Yazi's sorted/filtered entries and theme-resolved icons from live Files.
local function read_entries(folder, limit, base)
	local names, dirs, icons = {}, {}, {}
	for i = 1, math.min(limit or #folder.files, #folder.files) do
		local file = folder.files[i]
		local name = file.url.name or tostring(file.url):match("([^/]+)$") or tostring(file.url)
		names[i] = name
		dirs[name] = file.cha.is_dir
		local ok_icon, icon = pcall(read_icon_text, file, base)
		if ok_icon then
			-- False means a successful nil result; empty text stays authoritative.
			icons[name] = icon
		elseif not icon_error_reported then
			icon_error_reported = true
			broadcast(KIND_STATE, { error = tostring(icon) })
		end
		-- An API error leaves this entry's icon metadata unknown, not disabled.
	end
	return names, dirs, icons
end

-- A partial or failed folder listing is not an authoritative empty directory.
local function read_entry_count(folder)
	local ok, count = pcall(function()
		local loaded, err = folder.stage()
		if loaded == true and err == nil then return #folder.files end
	end)
	return ok and count or nil
end

local function read_folder(base)
	local cur = cx.active.current
	local names, dirs, icons = read_entries(cur, nil, base)
	return { url = tostring(cur.cwd), files = names, file_dirs = dirs, file_icons = icons, file_count = read_entry_count(cur), selected = selected_count() }
end

local function read_parent(base)
	local p = cx.active.parent
	if not p or not p.cwd then return nil end
	local names, dirs, icons = read_entries(p, 30, base)
	return { cwd = tostring(p.cwd), files = names, file_dirs = dirs, file_icons = icons, file_count = read_entry_count(p) }
end

local function read_preview(hovered, base)
	local p = cx.active.preview.folder
	if not p or not hovered.dir or tostring(p.cwd) ~= hovered.url then return nil end
	local names, dirs, icons = read_entries(p, 30, base)
	return { cwd = tostring(p.cwd), files = names, file_dirs = dirs, file_icons = icons, file_count = read_entry_count(p) }
end

local function read_icon_overrides(folder)
	local file = folder and folder.hovered
	if not file then return {} end
	local ok_icon, icon = pcall(read_icon_text, file)
	if not ok_icon then return {} end
	local name = file.url.name or tostring(file.url):match("([^/]+)$") or tostring(file.url)
	return { [name] = icon }
end


-- Hover metadata may change without cursor movement; dedupe the complete payload.
local last_hover = nil
local function publish_hover()
	local payload = read_hovered()
	local key = ya.json_encode(payload)
	if key ~= last_hover then
		last_hover = key
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
local listing_epoch = nil
local listing_revision = 0
local last_listing_key = nil

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

local function write_inbox(sdir, cid, kind, encoded)
	local tmp_file = string.format("%s/%s-%s.tmp", sdir, kind, cid)
	local dest_file = string.format("%s/%s-%s.json", sdir, kind, cid)
	local f = io.open(tmp_file, "w")
	if not f then return false end
	local written = f:write(encoded)
	local closed = f:close()
	return written ~= nil and closed == true and os.rename(tmp_file, dest_file) ~= nil
end

-- Append a small envelope without serializing the large listing a second time.
local function with_envelope(encoded, envelope)
	return encoded:sub(1, -2) .. "," .. ya.json_encode(envelope):sub(2)
end

local function dump_state()
	local active = cx.active
	if not active or not active.current then
		return
	end

	local folder = read_folder(true)
	local hovered = read_hovered()
	local parent = read_parent(true)
	local preview = read_preview(hovered, true)
	local ok_p, pulse_data = pcall(read_pulse)
	local tasks_data = ok_p and pulse_data.tasks or { total = 0, succ = 0, fail = 0, found = 0, processed = 0 }
	local selected = ok_p and pulse_data.selected or selected_count()
	local selected_paths = selected_urls()
	local marked = {}
	-- File:is_marked() is always zero in normal mode; avoid a redundant full walk.
	if not active.mode.is_normal then
		for i = 1, #active.current.files do
			local file = active.current.files[i]
			if file:is_marked() ~= 0 then marked[#marked + 1] = tostring(file.url) end
		end
	end
	local mode = active.mode.is_normal and "normal" or (active.mode.is_select and "select" or "unset")
	local ok_filter, filter = pcall(function()
		local value = active.current.files.filter
		return value and tostring(value) or false
	end)
	if not ok_filter then filter = nil end
	local ok_finder, finder = pcall(function()
		local value = active.finder
		return value and tostring(value) or false
	end)
	if not ok_finder then finder = nil end

	local listing = {
		cwd = folder.url,
		files = folder.files,
		file_dirs = folder.file_dirs,
		file_icons = folder.file_icons,
		file_count = folder.file_count,
		parent = parent,
		preview = preview,
		selected_urls = selected_paths,
		marked_urls = marked,
	}
	local ok_listing, listing_key = pcall(ya.json_encode, listing)
	if not ok_listing or not listing_key then return end
	local sdir, cid = state_dir(), tostring(get_client_id())
	listing_epoch = listing_epoch or tostring(ya.time()) .. ":" .. tostring({})
	if listing_key ~= last_listing_key then
		local revision = listing_revision + 1
		local encoded = with_envelope(listing_key, { epoch = listing_epoch, revision = revision })
		-- The consumer accepts only matching epoch/revision pairs, never mixed files.
		if not write_inbox(sdir, cid, "listing", encoded) then return end
		last_listing_key, listing_revision = listing_key, revision
	end
	local payload = {
		listing_epoch = listing_epoch,
		listing_revision = listing_revision,
		cursor = active.current.cursor + 1,
		icon_overrides = read_icon_overrides(active.current),
		parent_icon_overrides = parent and read_icon_overrides(active.parent) or nil,
		preview_icon_overrides = preview and read_icon_overrides(active.preview.folder) or nil,
		filter = filter,
		finder = finder,
		selected = selected,
		mode = mode,
		hovered = hovered.url and hovered or nil,
		tasks = tasks_data,
	}
	local ok_encoded, key = pcall(ya.json_encode, payload)
	if not ok_encoded or not key or key == last_dump_key then return end
	local seq = state_seq + 1
	local encoded = with_envelope(key, { ts = ya.time(), seq = seq, client_id = cid })
	if write_inbox(sdir, cid, "state", encoded) then
		last_dump_key, state_seq = key, seq
	end
end

local function reset_directory_usage()
	directory_usage.token = directory_usage.token + 1
	if directory_usage.handle then directory_usage.handle:abort() end
	directory_usage.handle = nil
	directory_usage.url, directory_usage.bytes, directory_usage.attempted = nil, nil, false
end

local function owns_directory_usage(url, token)
	if directory_usage.url ~= url or directory_usage.token ~= token then return false end
	local h = cx.active.current.hovered
	local cha = h and h.cha
	return h ~= nil and tostring(h.url) == url and cha ~= nil and cha.is_dir
		and not cha.is_dummy and not cha.is_link
		and cha.mtime == directory_usage.mtime and cha.btime == directory_usage.btime
		and cha.len == directory_usage.len
end

local usage_current = ya.sync(function(_, url, token)
	local ok, current = pcall(owns_directory_usage, url, token)
	return ok and current or false
end)

local usage_complete = ya.sync(function(_, url, token, bytes)
	pcall(function()
		if not owns_directory_usage(url, token) then return end
		directory_usage.handle, directory_usage.bytes = nil, bytes
		-- Completion is metadata, not a cursor move; publish even without a redraw.
		publish_hover()
		dump_state()
	end)
end)

directory_size = function(h)
	local cha = h and h.cha
	-- Yazi's calculator does not follow a root symlink; never export its inode.
	if not cha or not cha.is_dir or cha.is_dummy or cha.is_link then
		if directory_usage.url then reset_directory_usage() end
		return nil
	end
	local url = tostring(h.url)
	if directory_usage.url ~= url or directory_usage.mtime ~= cha.mtime
		or directory_usage.btime ~= cha.btime or directory_usage.len ~= cha.len then
		reset_directory_usage()
		directory_usage.url = url
		directory_usage.mtime, directory_usage.btime, directory_usage.len = cha.mtime, cha.btime, cha.len
	end
	-- File:size() reads Yazi's directory cache; it never returns Cha.len for dirs.
	local ok_cached, cached = pcall(function() return h:size() end)
	if ok_cached and type(cached) == "number" and cached >= 0 then
		if directory_usage.handle then
			directory_usage.token = directory_usage.token + 1
			directory_usage.handle:abort()
			directory_usage.handle = nil
		end
		directory_usage.bytes, directory_usage.attempted = cached, true
		return cached
	end
	if not directory_usage.attempted then
		directory_usage.attempted = true
		local token = directory_usage.token
		directory_usage.handle = ya.async(function()
			local ok, bytes = pcall(function()
				if not usage_current(url, token) then return nil end
				-- Public 26.9.1 fs.calc_size uses Yazi's async, chunked Rust walker.
				local calculator, err = fs.calc_size(Url(url))
				if not calculator or err then return nil end
				local root = calculator.cha
				if not root or not root.is_dir or root.is_dummy or root.is_link then return nil end
				local total = 0
				while true do
					if not usage_current(url, token) then return nil end
					local chunk, read_err = calculator:recv()
					if read_err then return nil end
					if chunk == nil then return total end
					if type(chunk) ~= "number" or chunk < 0 then return nil end
					total = total + chunk
				end
			end)
			usage_complete(url, token, ok and bytes or nil)
		end)
	end
	return directory_usage.bytes
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
	if #result == 0 then
		local hovered = cx.active.current.hovered
		local path = target or (hovered and tostring(hovered.url))
		if path then result[1] = path end
	end
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

-- Quote transport only; Yazi's shell actor owns all template/context semantics.
local function shell_quote(value)
	return "'" .. value:gsub("'", "'\\''") .. "'"
end

local SHELL_PREP_LIMIT = 256 * 1024

local function read_shell_prep(path)
	local f, err = io.open(path, "rb")
	assert(f, tostring(err))
	local ok, data = pcall(f.read, f, SHELL_PREP_LIMIT + 1)
	f:close()
	assert(ok, tostring(data))
	data = data or ""
	assert(#data <= SHELL_PREP_LIMIT, "Native shell preparation exceeds 256 KiB")
	return data
end

local function prepare_shell(req, run)
	local directory = state_dir() .. "/prep-" .. req.id
	local created, create_err = fs.create("dir", Url(directory))
	assert(created, "Native shell preparation failed: " .. tostring(create_err))
	local ok, result = pcall(function()
		-- A request-specific delimiter is never chosen from user script content.
		-- This transport is not a sandbox against a malicious same-user filesystem.
		local delimiter = "TERN_PREP_" .. ya.hash(req.id .. tostring(ya.time()) .. tostring(math.random()))
		while run:find(delimiter, 1, true) do delimiter = delimiter .. "_" end
		local function path(name)
			-- The wrapper itself also passes through native percent expansion.
			return (shell_quote(directory .. "/" .. name):gsub("%%", "%%%%"))
		end
		local wrapper = table.concat({
			"umask 077",
			"set -e",
			"cat > " .. path("run") .. " <<'" .. delimiter .. "'",
			run,
			delimiter,
			"printf '%%s\\0' \"$0\" \"$@\" > " .. path("args"),
			"pwd > " .. path("cwd"),
			"printf 'done' > " .. path("done"),
		}, "\n")
		-- Only the preparation script executes here, never the user's script.
		-- The native actor performs visual escape and captures its own argv/cwd.
		ya.exec("shell", { wrapper, block = false })
		local deadline = ya.time() + 5
		local ready = false
		for _ = 1, 250 do
			local marker = io.open(directory .. "/done", "rb")
			if marker then
				marker:close()
				ready = true
				break
			end
			if ya.time() >= deadline then break end
			ya.sleep(0.02)
		end
		assert(ready, "Native shell preparation timed out; user script was not executed")
		local expanded = read_shell_prep(directory .. "/run")
		local argv = read_shell_prep(directory .. "/args")
		local cwd = read_shell_prep(directory .. "/cwd")
		assert(expanded:sub(-1) == "\n" and cwd:sub(-1) == "\n", "Incomplete native shell preparation")
		-- Strip exactly the newline added by the heredoc/pwd, not script/path data.
		expanded, cwd = expanded:sub(1, -2), cwd:sub(1, -2)
		local command = { "sh", "-c", shell_quote(expanded) }
		local pos = 1
		while pos <= #argv do
			local ending = assert(argv:find("\0", pos, true), "Incomplete native shell arguments")
			command[#command + 1] = shell_quote(argv:sub(pos, ending - 1))
			pos = ending + 1
		end
		assert(#command > 3 and cwd ~= "", "Missing native shell context")
		return { command = table.concat(command, " "), cwd = cwd, block = true }
	end)
	-- Revoke the original pathname first: a delayed native task cannot recreate
	-- staging after timeout, even if it already opened one of the removed files.
	local retired = directory .. ".cleanup"
	local renamed, rename_err = fs.rename(Url(directory), Url(retired))
	if not renamed and rename_err and rename_err.kind == "NotFound" then
		-- The companion may already have removed staging while closing.
		assert(ok, "Native shell preparation failed: " .. tostring(result))
		return result
	end
	local removed, remove_err = fs.remove("dir_all", Url(renamed and retired or directory))
	assert(removed or (remove_err and remove_err.kind == "NotFound"),
		"Native shell preparation cleanup failed: " .. tostring(remove_err))
	assert(renamed, "Native shell preparation cleanup failed: " .. tostring(rename_err))
	assert(ok, "Native shell preparation failed: " .. tostring(result))
	return result
end

local companion_actors = {}
for name in ("cd arrow leave enter back forward reveal yank toggle toggle_all visual_mode escape hidden find_arrow"):gmatch("%S+") do
	companion_actors[name] = true
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
		ya.emit("remove", { force = true }) -- Trash only; Tern confirmed these paths.
		ya.emit("plugin", { "tern", "settled", mode = "sync" })
		return
	elseif stage == "settled" then
		local req = assert(self.pending)
		self.pending = nil
		last_dump_key = nil
		dump_state()
		reply(req, { ok = true, paths = req.paths, queued = true })
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
			assert(req.action == "shell", "Command input only accepts native shell scripts")
			local run = req.args.run or req.args[1]
			assert(type(run) == "string" and run ~= "", "Supply a shell script in the native command input")
			assert(not req.args.interactive, "Local command input does not open another interactive Yazi prompt")
			assert(not req.args.cwd, "Shell runs in Yazi's current directory; use cd first")
			assert(req.args.block == nil or type(req.args.block) == "boolean", "--block is a boolean flag")
			assert(req.args.orphan == nil or type(req.args.orphan) == "boolean", "--orphan is a boolean flag")
			if os.getenv("TERN_YAZI_BRIDGE") then
				-- Acknowledgement is acceptance, not completion of the blocking actor.
				reply(req, { ok = true, queued = true, orphan = req.args.orphan == true })
				ya.exec("shell", req.args)
			elseif req.args.orphan or not req.args.block then
				ya.exec("shell", req.args)
				reply(req, { ok = true, queued = true, orphan = req.args.orphan == true })
			else
				reply(req, { ok = true, shell = prepare_shell(req, run) })
			end
			return
		end
		if req.op == "ping" then
			snapshot()
			reply(req, { ok = true })
		elseif req.op == "trash_prepare" then
			reply(req, { ok = true, paths = targets(req.target) })
		elseif req.op == "trash_commit" then
			assert(type(req.paths) == "table" and #req.paths > 0, "No targets")
			table.sort(req.paths)
			local files = {}
			for _, path in ipairs(req.paths) do
				local file, e = fs.file(Url(path))
				assert(file, tostring(e))
				files[#files + 1] = file
			end
			commit(req, files)
		elseif req.op == "toggle_advance" then
			-- Each exec awaits its actor before the next one is dispatched.
			ya.exec("toggle", {})
			ya.exec("arrow", { 1 })
			snapshot()
			reply(req, { ok = true })
		elseif req.op == "cd" then
			assert(type(req.target) == "string" and req.target ~= "", "Missing directory path")
			local target = Url(req.target)
			local file, e = fs.file(target)
			assert(file, tostring(e))
			assert(file.cha.is_dir, "Target is not a directory")
			ya.exec("cd", { target })
			snapshot()
			reply(req, { ok = true })
		elseif req.op == "activate" then
			assert(req.action == "enter", "Unsupported activation action")
			ya.exec("reveal", { Url(assert(req.target)), no_dummy = true })
			ya.exec(req.action, {})
			snapshot()
			reply(req, { ok = true, queued = true })
		elseif req.op == "filter" then
			ya.exec("filter_do", { req.query or "", smart = true, done = req.done ~= false })
			snapshot()
			reply(req, { ok = true })
		elseif req.op == "find" then
			assert(type(req.query) == "string", "Find requires a query")
			assert(req.previous == nil or type(req.previous) == "boolean", "--previous is a boolean flag")
			ya.exec("find_do", { req.query, smart = true, previous = req.previous == true })
			snapshot()
			reply(req, { ok = true })
		elseif req.op == "action" then
			assert(companion_actors[req.action], "Unsupported companion action")
			ya.exec(req.action, req.args or {})
			snapshot()
			reply(req, { ok = true, queued = true })
		else
			error("Unknown companion operation")
		end
	end)
	if not ok then reply(req, { ok = false, rejected = true, error = tostring(err) }) end
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
