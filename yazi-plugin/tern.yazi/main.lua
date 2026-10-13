-- tern.yazi: exports yazi's state to the tern-yazi launcher through atomic
-- snapshot files in the runtime inbox, and executes its requests (`ya emit-to
-- <client-id> plugin tern <json>`). Zero patches: plugin API only.
--
-- Install: link this directory into ~/.config/yazi/plugins/, then add
--   require("tern"):setup()
-- to ~/.config/yazi/init.lua.

local M = {}

-- ps.sub callbacks run in the sync context after the manager settles
-- (yazi publishes hover/cd post-update), so read cx directly here.
local function selected_count()
	return #cx.active.selected
end

-- Search results live under `search://` URLs; the launcher reads, opens and previews the real
-- paths, so every exported URL is the local one.
local function real(url)
	return url.is_search and tostring(url.path) or tostring(url)
end

-- A listed entry's name: its path below a search folder, as Yazi shows it, else its name.
local function entry_name(file, cwd)
	if cwd and cwd.is_search then
		local path, base = tostring(file.url.path), tostring(cwd.path):gsub("/+$", "") .. "/"
		if path:sub(1, #base) == base then return path:sub(#base + 1) end
		return path
	end
	return file.url.name or tostring(file.url):match("([^/]+)$") or tostring(file.url)
end

local function selected_urls()
	local urls = {}
	for _, file in pairs(cx.active.selected) do
		urls[#urls + 1] = real(file.url)
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
		url = real(h.url),
		dir = cha and cha.is_dir or false,
		size = size,
		mtime = mtime and math.floor(mtime) or nil,
		permissions = ok_perm and type(permissions) == "string" and permissions ~= "" and permissions or nil,
		selected = selected_count(),
	}
end

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
		local name = entry_name(file, folder.cwd)
		names[i] = name
		dirs[name] = file.cha.is_dir
		local ok_icon, icon = pcall(read_icon_text, file, base)
		if ok_icon then
			-- False means a successful nil result; empty text stays authoritative.
			icons[name] = icon
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
	return {
		url = real(cur.cwd),
		search = cur.cwd.is_search and tostring(cur.cwd.domain) or nil,
		files = names, file_dirs = dirs, file_icons = icons, file_count = read_entry_count(cur), selected = selected_count(),
	}
end

-- The tab's linemode and the text of each current-folder row, from Yazi's own `Linemode`
-- component so user overrides apply. `none` and `solo` draw nothing; an unknown mode shows
-- its name, as Yazi does.
local function read_linemode()
	local mode = cx.active.pref.linemode
	if mode == "none" or mode == "solo" then return mode, nil end
	local files = cx.active.current.files
	local texts = {}
	local builder = Linemode[mode]
	for i = 1, #files do
		local ok, text = true, " " .. mode
		if builder then ok, text = pcall(builder, Linemode:new(files[i])) end
		texts[i] = ok and type(text) == "string" and text or ""
	end
	return mode, texts
end

local function read_parent(base)
	local p = cx.active.parent
	if not p or not p.cwd then return nil end
	local names, dirs, icons = read_entries(p, 30, base)
	return { cwd = real(p.cwd), files = names, file_dirs = dirs, file_icons = icons, file_count = read_entry_count(p) }
end

local function read_preview(hovered, base)
	local p = cx.active.preview.folder
	if not p or not hovered.dir or real(p.cwd) ~= hovered.url then return nil end
	local names, dirs, icons = read_entries(p, 30, base)
	return { cwd = real(p.cwd), files = names, file_dirs = dirs, file_icons = icons, file_count = read_entry_count(p) }
end

local function read_icon_overrides(folder)
	local file = folder and folder.hovered
	if not file then return {} end
	local ok_icon, icon = pcall(read_icon_text, file)
	if not ok_icon then return {} end
	return { [entry_name(file, folder.cwd)] = icon }
end


-- Aggregate task state: totals come from cx.tasks.summary (status-bar component
-- source), byte counters summed over cx.tasks.snaps. Callers wrap it in pcall so a
-- shape change in Yazi degrades to empty counters. Yazi fills snaps only while its task
-- manager is open; they are exported as that manager's rows.
local function read_tasks()
	local summary = cx.tasks.summary
	local found, processed = 0, 0
	local rows = {}
	for i, snap in ipairs(cx.tasks.snaps) do
		local p = snap.prog or {}
		if p.total_bytes then
			found = found + p.total_bytes
			processed = processed + (p.processed_bytes or 0)
		end
		rows[i] = {
			kind = p.kind,
			title = snap.title,
			percent = snap.percent,
			cooked = snap.cooked,
			running = snap.running,
			failed = snap.failed,
			processed_bytes = p.processed_bytes,
			total_bytes = p.total_bytes,
			success_files = p.success_files,
			failed_files = p.failed_files,
			total_files = p.total_files,
		}
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
		manager = { cursor = cx.tasks.cursor, items = rows },
	}
end

-- Yazi's recent notifications (`ya.notify`, task failures, ...), captured from the local
-- `relay-notify-push` event. Ids only grow; the launcher shows each id once.
local NOTICE_KEEP = 8
local notices, notice_seq = {}, 0

-- State inbox writer: atomic state and listing snapshots for the launcher.
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
	local ok_p, task_data = pcall(read_tasks)
	local tasks_data = ok_p and task_data.tasks or { total = 0, succ = 0, fail = 0, found = 0, processed = 0 }
	local selected = ok_p and task_data.selected or selected_count()
	local selected_paths = selected_urls()
	local marked = {}
	-- File:is_marked() is always zero in normal mode; avoid a redundant full walk.
	if not active.mode.is_normal then
		for i = 1, #active.current.files do
			local file = active.current.files[i]
			if file:is_marked() ~= 0 then marked[#marked + 1] = real(file.url) end
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

	local ok_linemode, linemode, linemode_text = pcall(read_linemode)
	if not ok_linemode then linemode, linemode_text = nil, nil end

	local listing = {
		cwd = folder.url,
		search = folder.search,
		files = folder.files,
		file_dirs = folder.file_dirs,
		file_icons = folder.file_icons,
		file_count = folder.file_count,
		linemode = linemode,
		linemode_text = linemode_text,
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
	local tabs_data = nil
	local ok_tabs, tabs = pcall(function() return cx.tabs end)
	if ok_tabs and tabs then
		local count = #tabs
		-- Yazi draws the tab bar from two tabs on; a renamed lone tab shows its name too.
		-- `tab.name` falls back to the directory name; `tab.pref.name` is the explicit one.
		local function explicit(t)
			local ok, name = pcall(function() return t.pref.name end)
			return ok and type(name) == "string" and name or ""
		end
		local lone = count == 1 and explicit(tabs[1]) ~= ""
		if count > 1 or lone then
			local list = {}
			for i = 1, count do
				local t = tabs[i]
				local t_cwd = t and t.current and t.current.cwd
				local t_name = t and t.name
				if not t_name or t_name == "" then
					t_name = t_cwd and (t_cwd.name or tostring(t_cwd):match("([^/]+)$")) or tostring(i)
				end
				list[i] = {
					name = t_name,
					-- The name set by `tab_rename`, empty while the tab follows its directory.
					custom = t and explicit(t) or "",
					active = (i == tabs.idx),
				}
			end
			tabs_data = {
				idx = tabs.idx,
				total = count,
				items = list,
			}
		end
	end
	local payload = {
		listing_epoch = listing_epoch,
		listing_revision = listing_revision,
		cursor = active.current.cursor + 1,
		preview_skip = active.preview.skip,
		icon_overrides = read_icon_overrides(active.current),
		parent_icon_overrides = parent and read_icon_overrides(active.parent) or nil,
		preview_icon_overrides = preview and read_icon_overrides(active.preview.folder) or nil,
		filter = filter,
		finder = finder,
		selected = selected,
		mode = mode,
		hovered = hovered.url and hovered or nil,
		tasks = tasks_data,
		task_manager = ok_p and task_data.manager or nil,
		tabs = tabs_data,
		notices = #notices > 0 and notices or nil,
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
	return h ~= nil and real(h.url) == url and cha ~= nil and cha.is_dir
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
		-- Completion is metadata, not a cursor move; snapshot even without a redraw.
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
	local url = real(h.url)
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

local current_cwd = ya.sync(function()
	local ok, cwd = pcall(function() return real(cx.active.current.cwd) end)
	assert(ok, "Current directory unavailable")
	return cwd
end)

-- The listed destination's metadata, or nil when the path is free.
local function existing_target(target)
	local cha, err = fs.cha(target)
	if cha then return cha end
	assert(err and err.kind == "NotFound", tostring(err))
end

local function rename_helper()
	local bridge = os.getenv("TERN_YAZI_BRIDGE")
	if bridge then return assert(ya.json_decode(bridge).helper, "Missing managed helper") end
	local configured = os.getenv("TERN_YAZI_LAUNCHER_PATH")
	if configured then return configured end
	for directory in (os.getenv("PATH") or ""):gmatch("[^:]+") do
		if directory:sub(1, 1) == "/" then
			local candidate = directory .. "/../libexec/tern-yazi-launch"
			local cha = fs.cha(Url(candidate))
			if cha and not cha.is_dir then return candidate end
		end
	end
	return (os.getenv("HOME") or "") .. "/.local/libexec/tern-yazi-launch"
end

-- The listed entry for a real path: under a search folder it is a `search://` URL, which
-- `reveal` needs to stay in the search.
local entry_url = ya.sync(function(_, path)
	local ok, found = pcall(function()
		local files = cx.active.current.files
		for i = 1, #files do
			if real(files[i].url) == path then return tostring(files[i].url) end
		end
	end)
	return ok and found or nil
end)

-- Reveals a real path, as the listed entry when there is one.
local function reveal(path)
	ya.exec("reveal", { Url(entry_url(path) or path), no_dummy = true })
end

-- The watcher updates Yazi's listing asynchronously; revealing before the new
-- entry exists leaves the cursor on an unrelated row.
local function reveal_listed(target)
	local url = tostring(target)
	for _ = 1, 40 do
		if entry_url(url) then break end
		ya.sleep(0.05)
	end
	reveal(url)
	snapshot()
end

local targets = ya.sync(function(_, target)
	local paths = {}
	for _, file in pairs(cx.active.selected) do paths[real(file.url)] = true end
	for i = 1, #cx.active.current.files do
		local f = cx.active.current.files[i]
		local marked = f:is_marked()
		if marked == 1 then paths[real(f.url)] = true
		elseif marked == 2 then paths[real(f.url)] = nil end
	end
	local result = {}
	for path in pairs(paths) do result[#result + 1] = path end
	if #result == 0 then
		local hovered = cx.active.current.hovered
		local path = target or (hovered and real(hovered.url))
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

local request_actors = {}
for name in ("cd arrow leave enter back forward reveal yank toggle toggle_all visual_mode escape hidden find_arrow paste unyank link hardlink seek tab_create tab_switch tab_swap tab_close close plugin open sort linemode follow tab_rename"):gmatch("%S+") do
	request_actors[name] = true
end
-- Plugins the browser's keys run, as Yazi's default keymap does (`z` fzf, `Z` zoxide).
local request_plugins = { fzf = true, zoxide = true }
local task_actions = { show = true, close = true, arrow = true, inspect = true, cancel = true }

-- Whether Yazi has selected or visually marked files for a bulk rename to act on.
local has_selection = ya.sync(function()
	if #cx.active.selected > 0 then return true end
	if not cx.active.mode.is_normal then
		for i = 1, #cx.active.current.files do
			if cx.active.current.files[i]:is_marked() == 1 then return true end
		end
	end
	return false
end)

local targets_hovered = ya.sync(function()
	local hovered = cx.active.current.hovered
	return hovered and real(hovered.url) or nil
end)

-- Yazi's mime type per file (`folder/local` for directories), normalized like its own
-- `mime-local` fetcher. Yazi keeps the result off the Lua API, so ask `file` again.
local function mimes_of(paths, files)
	local mimes, listed = {}, {}
	for i, file in ipairs(files) do
		if file.cha.is_dir then mimes[i] = "folder/local" else listed[#listed + 1] = i end
	end
	if #listed == 0 then return mimes end
	local args = {}
	for _, i in ipairs(listed) do args[#args + 1] = paths[i] end
	local program = os.getenv("YAZI_FILE_ONE") or "file"
	local output = Command(program):arg({ "-bL", "--mime-type", "--" }):arg(args):stdout(Command.PIPED):output()
	local lines = {}
	if output then
		for line in output.stdout:gmatch("[^\n]+") do lines[#lines + 1] = line end
	end
	for n, i in ipairs(listed) do
		local typ, sub = (lines[n] or ""):match("^(%a[%w%-]*)/([%w%.%+%-]+)%s*$")
		if typ then
			typ = typ:gsub("^x%-", "", 1)
			sub = sub:gsub("^x%-", "", 1)
			sub = sub:gsub("^vnd%.", "", 1)
			mimes[i] = typ .. "/" .. sub
		else
			mimes[i] = "application/octet-stream"
		end
	end
	return mimes
end

-- The openers every target accepts, in Yazi's order: rows of { id, description, command }.
-- Mirrors `Open::match_common`: the first matching `[open]` rule of each target, intersected.
local function openers_for(paths)
	local files = {}
	for i, path in ipairs(paths) do
		local file, e = fs.file(Url(path))
		assert(file, tostring(e))
		files[i] = file
	end
	local mimes = mimes_of(paths, files)
	local common
	for i, file in ipairs(files) do
		local names
		for _, rule in pairs(rt.open:match({ file = file, mime = mimes[i] })) do
			names = {}
			for _, name in ipairs(rule["use"]) do names[#names + 1] = name end
			break
		end
		if not names then return {} end
		if not common then
			common = names
		else
			local kept = {}
			for _, name in ipairs(common) do
				for _, other in ipairs(names) do
					if other == name then kept[#kept + 1] = name break end
				end
			end
			common = kept
		end
	end
	local rows, seen = {}, {}
	for _, name in ipairs(common or {}) do
		local rules = rt.opener[name]
		if rules then
			for _, rule in pairs(rules:match()) do
				local key = tostring(rule.run) .. "\0" .. tostring(rule.desc)
				if not seen[key] then
					seen[key] = true
					local desc = rule.desc
					if desc == nil or desc == "" then desc = tostring(rule.run):match("^%S+") or "" end
					rows[#rows + 1] = { tostring(rule.id), desc, rule.run }
				end
			end
		end
	end
	return rows
end

local function find_opener(id)
	for _, rules in pairs(rt.opener) do
		for _, rule in pairs(rules:match()) do
			if tostring(rule.id) == id then return rule end
		end
	end
end

local function shell_quote(text)
	local escaped = text:gsub("'", "'\\''")
	return "'" .. escaped .. "'"
end

-- Quoted for the shell and shielded from the second `%` pass the `shell` actor makes.
local function splat_quote(text)
	local escaped = shell_quote(text):gsub("%%", "%%%%")
	return escaped
end

local function dirname(path)
	local dir = path:match("^(.*)/[^/]*$")
	if dir == nil then return "." end
	return dir == "" and "/" or dir
end

-- Expands an opener's `run` like Yazi's `Splatter` over `paths`: `%s`/`%S` the files,
-- `%d`/`%D` their directories (an optional digit picks one), `%h`/`%H` no hovered file.
-- Other `%` sequences are left for the `shell` actor's own pass.
local function expand_opener(run, paths)
	local out, i = {}, 1
	while i <= #run do
		local c, n = run:sub(i, i), run:sub(i + 1, i + 1)
		if c == "%" and n:match("[sSdD]") then
			local digits = run:match("^%d+", i + 2) or ""
			local index = tonumber(digits)
			local items = {}
			for k, path in ipairs(paths) do
				if not index or index == k then
					items[#items + 1] = splat_quote((n == "d" or n == "D") and dirname(path) or path)
				end
			end
			if index and #items == 0 then items[1] = "''" end
			out[#out + 1] = table.concat(items, " ")
			i = i + 2 + #digits
		elseif c == "%" and n:match("[hH]") then
			out[#out + 1] = "''"
			i = i + 2
		elseif c == "%" and n ~= "" then
			out[#out + 1] = c .. n
			i = i + 2
		else
			out[#out + 1] = c
			i = i + 1
		end
	end
	return table.concat(out)
end

-- One command for all targets when `run` spreads (`%s` without a digit), else one each.
local function opener_commands(run, paths)
	local spread, i = false, 1
	while i <= #run do
		if run:sub(i, i) == "%" then
			if run:sub(i + 1, i + 1):match("[sSdD]") and not run:match("^%d", i + 2) then spread = true end
			i = i + 2
		else
			i = i + 1
		end
	end
	if spread then return { expand_opener(run, paths) } end
	local commands = {}
	for _, path in ipairs(paths) do commands[#commands + 1] = expand_opener(run, { path }) end
	return commands
end

-- The rows Yazi's spotter shows for a file (`file.lua` `spot_base`, plus `image.lua`'s
-- section): { heading } or { label, value }.
local function spot_rows(path)
	local url = Url(path)
	local file, e = fs.file(url)
	assert(file, tostring(e))
	local cha = file.cha
	local mime = mimes_of({ path }, { file })[1]
	local pair = { file = file, mime = mime }
	local function names(set)
		local out = {}
		for _, v in pairs(set:match(pair)) do out[#out + 1] = v.name end
		return #out > 0 and table.concat(out, ", ") or "-"
	end
	local function first(set)
		for _, v in pairs(set:match(pair)) do return v.name end
		return "-"
	end
	local function date(time)
		return time and os.date("%Y-%m-%d %H:%M:%S", math.floor(time)) or "-"
	end
	local rows = {
		{ "Base" },
		{ "Created:", date(cha.btime) },
		{ "Modified:", date(cha.mtime) },
		{ "Mimetype:", mime },
	}
	if not cha.is_dir then rows[#rows + 1] = { "Size:", ya.readable_size(cha.len) } end
	rows[#rows + 1] = { "Permissions:", cha:perm() or "-" }
	local link = fs.cha(url, false)
	if link and link.is_link then
		local out = Command("readlink"):arg({ "--", path }):stdout(Command.PIPED):output()
		if out then rows[#rows + 1] = { "Link to:", (out.stdout:gsub("\n$", "")) } end
	end
	if mime:sub(1, 6) == "image/" then
		local ok, info = pcall(ya.image_info, url)
		if ok and info then
			rows[#rows + 1] = { "Image" }
			rows[#rows + 1] = { "Format:", tostring(info.format) }
			rows[#rows + 1] = { "Size:", string.format("%dx%d", info.w, info.h) }
			rows[#rows + 1] = { "Color:", tostring(info.color) }
		end
	end
	rows[#rows + 1] = { "Plugins" }
	rows[#rows + 1] = { "Spotter:", first(rt.plugin.spotters) }
	rows[#rows + 1] = { "Previewer:", first(rt.plugin.previewers) }
	rows[#rows + 1] = { "Fetchers:", names(rt.plugin.fetchers) }
	rows[#rows + 1] = { "Preloaders:", names(rt.plugin.preloaders) }
	return rows
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
		-- Tern confirmed these paths: trash them, or delete them for good when it asked so.
		ya.emit("remove", { force = true, permanently = req.permanent == true })
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
			assert(req.action == "shell", "Command input only accepts shell scripts")
			local run = req.args.run or req.args[1]
			assert(type(run) == "string" and run ~= "", "Supply a shell script in the command input")
			assert(not req.args.interactive, "Command input does not open another interactive Yazi prompt")
			assert(not req.args.cwd, "Shell runs in Yazi's current directory; use cd first")
			assert(req.args.block == nil or type(req.args.block) == "boolean", "--block is a boolean flag")
			assert(req.args.orphan == nil or type(req.args.orphan) == "boolean", "--orphan is a boolean flag")
			-- Acknowledgement is acceptance, not completion. A blocking shell leaves Yazi's
			-- alternate screen; the launcher then lends the pane to Yazi's terminal.
			reply(req, { ok = true, queued = true, orphan = req.args.orphan == true })
			ya.exec("shell", req.args)
			return
		end
		if req.op == "ping" then
			snapshot()
			reply(req, { ok = true })
		elseif req.op == "trash_prepare" then
			reply(req, { ok = true, paths = targets(req.target) })
		elseif req.op == "trash_commit" then
			assert(type(req.paths) == "table" and #req.paths > 0, "No targets")
			assert(req.permanent == nil or type(req.permanent) == "boolean", "--permanent is a boolean flag")
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
			reveal(assert(req.target))
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
		elseif req.op == "quit" then
			-- Yazi's `mgr:quit` asks for confirmation through a popup, which Yazi draws on its
			-- hidden terminal. The launcher asks here instead: unfinished tasks are reported back
			-- and nothing quits until `confirmed`; otherwise `app:quit` (Yazi's popup-free quit,
			-- which writes --cwd-file unless `no_cwd_file`) runs directly. Yazi's own check is
			-- `ongoing.len()`; `summary.total` is the closest Lua-visible count: it matches
			-- `ongoing` except for zero-total tasks that have not failed. Titles exist only
			-- while the task manager is open (Yazi fills `snaps` then). The launcher runs this
			-- as a sync entry so the request is not itself an ongoing task.
			assert(req.no_cwd_file == nil or type(req.no_cwd_file) == "boolean", "no_cwd_file is a boolean flag")
			local left, titles = 0, {}
			if not req.confirmed then
				left = cx.tasks.summary.total
				for i, snap in ipairs(cx.tasks.snaps) do
					if i > 10 then break end
					titles[i] = snap.title
				end
			end
			if left > 0 then
				reply(req, { ok = true, tasks = left, titles = titles })
			else
				reply(req, { ok = true, quitting = true })
				ya.emit("app:quit", { no_cwd_file = req.no_cwd_file == true })
			end
		elseif req.op == "tasks" then
			-- Yazi's task manager actions (`w` opens it; `tasks:inspect` borrows the terminal).
			-- The launcher runs these as sync entries: an async run is itself a task and would
			-- shift `cx.tasks.cursor`. Redraws snapshot the result.
			assert(task_actions[req.action], "Unsupported task manager action")
			assert(req.step == nil or req.step == "prev" or req.step == "next", "Invalid task step")
			reply(req, { ok = true })
			ya.emit("tasks:" .. req.action, req.step and { req.step } or {})
		elseif req.op == "search" then
			assert(type(req.query) == "string" and req.query ~= "", "Search requires a query")
			assert(req.via == "fd" or req.via == "rg", "Search runs via fd or rg")
			-- Yazi runs the search and streams the results into a `search://` folder.
			ya.exec("search_do", { req.query, via = req.via })
			snapshot()
			reply(req, { ok = true })
		elseif req.op == "create" then
			assert(type(req.name) == "string" and req.name ~= "" and not req.name:find("%z"), "Creation requires a valid name")
			local is_dir = req.name:sub(-1) == "/"
			local clean_name = req.name:gsub("/+$", "")
			assert(clean_name ~= "", "Creation requires a name, not a directory separator")
			local target = Url(current_cwd()):join(clean_name)
			local existing = existing_target(target)
			if existing and req.overwrite ~= true then
				-- Yazi asks `Overwrite file?` here; the browser asks and retries with `overwrite`.
				reply(req, { ok = false, exists = true, target = tostring(target), error = "Destination already exists" })
				return
			end
			local parent = assert(target.parent, "Cannot create at root")
			local made, make_err = fs.create("dir_all", parent)
			assert(made, tostring(make_err))
			if is_dir then
				-- Like Yazi, an existing directory is kept as is.
				if not (existing and existing.is_dir) then
					local created, create_err = fs.create("dir", target)
					assert(created, tostring(create_err))
				end
			else
				if existing then
					assert(not existing.is_dir, "Cannot overwrite a directory with a file")
					local removed, remove_err = fs.remove("file", target)
					assert(removed, tostring(remove_err))
				end
				local fd, open_err = fs.access():write(true):create_new(true):open(target)
				assert(fd, tostring(open_err))
			end
			reveal_listed(target)
			reply(req, { ok = true, target = tostring(target) })
		elseif req.op == "rename" then
			assert(type(req.target) == "string" and req.target ~= "" and not req.target:find("%z"), "Rename requires a target")
			assert(type(req.new_name) == "string" and req.new_name ~= "" and not req.new_name:find("%z"), "Rename requires a new name")
			local source = Url(req.target:gsub("/+$", ""))
			local source_cha, source_err = fs.cha(source)
			assert(source_cha, tostring(source_err))
			local parent = assert(source.parent, "Cannot rename root")
			local target = parent:join(req.new_name)
			if source ~= target then
				local existing = existing_target(target)
				if existing and req.overwrite ~= true then
					reply(req, { ok = false, exists = true, target = tostring(target), error = "Destination already exists" })
					return
				end
				if existing then
					-- Confirmed overwrite: a plain rename replaces the destination.
					local renamed, rename_err = fs.rename(source, target)
					assert(renamed, tostring(rename_err))
				else
					local result, run_err = Command(rename_helper()):arg("--rename-no-replace")
						:arg(tostring(source)):arg(tostring(target)):output()
					assert(result, tostring(run_err))
					assert(result.status.success, result.stderr ~= "" and result.stderr or "Rename failed")
				end
			end
			reveal_listed(target)
			reply(req, { ok = true, target = tostring(target) })
		elseif req.op == "bulk_rename" then
			-- Yazi runs the bulk-rename opener (`$EDITOR`) on the terminal it lends to the pane.
			assert(has_selection(), "Bulk rename needs selected files")
			reply(req, { ok = true, queued = true })
			ya.exec("escape", { visual = true })
			ya.exec("bulk_rename", {})
		elseif req.op == "open_prepare" then
			local paths = targets(nil)
			local rows = {}
			if #paths > 0 then rows = openers_for(paths) end
			reply(req, { ok = true, paths = paths, rows = rows })
		elseif req.op == "open_with" then
			assert(type(req.paths) == "table" and #req.paths > 0, "No targets")
			for _, path in ipairs(req.paths) do
				assert(type(path) == "string" and path ~= "" and not path:find("%z"), "Invalid target")
			end
			local rule = find_opener(tostring(req.opener))
			assert(rule, "The opener is no longer configured")
			local commands = opener_commands(rule.run, req.paths)
			reply(req, { ok = true, queued = true })
			for _, command in ipairs(commands) do
				ya.exec("shell", { command, block = rule.block == true, orphan = rule.orphan == true })
			end
		elseif req.op == "spot" then
			local hovered = targets_hovered()
			assert(hovered, "Nothing to inspect")
			reply(req, { ok = true, paths = { hovered }, rows = spot_rows(hovered) })
		elseif req.op == "action" then
			assert(request_actors[req.action], "Unsupported action")
			if req.action == "tab_rename" then
				-- Interactive tab renaming would open an input Yazi draws on its hidden terminal.
				assert(type(req.args[1]) == "string" and not req.args.interactive, "Tab rename needs a name")
			end
			if req.action == "plugin" then
				assert(request_plugins[req.args[1]], "Unsupported plugin")
				-- fzf and zoxide borrow Yazi's terminal until the user picks; acknowledge first.
				reply(req, { ok = true, queued = true })
				ya.exec(req.action, req.args)
				return
			end
			if req.action == "reveal" and type(req.args[1]) == "string" then
				req.args[1] = Url(entry_url(req.args[1]) or req.args[1])
			end
			ya.exec(req.action, req.args or {})
			snapshot()
			reply(req, { ok = true, queued = true })
		else
			error("Unknown operation")
		end
	end)
	if not ok then reply(req, { ok = false, rejected = true, error = tostring(err) }) end
end

-- Yazi draws notifications on its hidden terminal. The local `relay-notify-push` handler
-- sees every one before Yazi keeps it; returning the body unchanged leaves Yazi's own
-- handling intact (nil would cancel the notification).
local function record_notice(body)
	pcall(function()
		notice_seq = notice_seq + 1
		notices[#notices + 1] = {
			id = notice_seq,
			title = tostring(body.title or ""),
			content = tostring(body.content or ""),
			level = tostring(body.level or "info"),
			timeout = tonumber(body.timeout) or 5,
		}
		if #notices > NOTICE_KEEP then table.remove(notices, 1) end
		dump_state()
	end)
	return body
end

function M:setup()
	pcall(os.execute, "mkdir -p " .. state_dir())
	-- Builtin local event bodies carry only { tab }; snapshot `cx` instead.
	ps.sub("hover", dump_state)
	ps.sub("cd", dump_state)
	ps.sub("relay-notify-push", record_notice)
	ps.sub("tab", dump_state)
	-- No pubsub event fires for selection changes without a hover move or for task
	-- progress on 26.9, but status children redraw together with the built-in progress
	-- gauge: snapshot from there (dump_state dedupes identical payloads).
	Status:children_add(function()
		dump_state()
		return ui.Line("")
	end, 1000, Status.RIGHT)
end

return M
