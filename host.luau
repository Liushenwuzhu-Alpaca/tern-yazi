-- host.luau: Host half of tern-yazi plugin.
-- Registers the "companion" block, reads Yazi state from File Inbox,
-- and renders an interactive UI tree with rich native previews (Markdown, SVG,
-- Mermaid, Image, Code highlighting), parent directory preview,
-- and robust keyboard navigation mapped to Yazi.

local ui = tern.ui
local running = {}
local launch_contexts = {}
local launches = {}

local LIST_IDS = {
	parent_list = "main.browser.columns.parent.parent_list",
	files_list = "main.browser.columns.current.files_list",
	child_list = "main.browser.columns.preview.child_list",
}

type HoverInfo = {
	url: string?,
	dir: boolean?,
	size: number?,
	mtime: number?,
	selected: number?,
}

type TaskInfo = {
	total: number,
	succ: number,
	fail: number,
	found: number?,
	processed: number?,
}

type ParentInfo = {
	cwd: string?,
	files: { string }?,
	file_dirs: { [string]: boolean }?,
}

type YaziState = {
	seq: number,
	client_id: any,
	parent: ParentInfo?,
	preview: ParentInfo?,
	cwd: string,
	files: { string },
	file_dirs: { [string]: boolean }?,
	filter: (string | boolean)?,
	finder: (string | boolean)?,
	selected: number,
	selected_urls: { string }?,
	mode: string?,
	marked_urls: { string }?,
	hovered: HoverInfo?,
	tasks: TaskInfo?,
}

-- ---------------------------------------------------------------- helpers

local function clickable(node: any, act: string): any
	node.p = node.p or {}
	node.p.actions = { click = act }
	return node
end

local function state_dir(): string
	local dir = tern.getenv("XDG_RUNTIME_DIR")
	if dir and dir ~= "" then
		return dir .. "/tern-yazi"
	end
	return "/tmp/tern-yazi"
end

local function fmt_size(bytes: number?): string
	if not bytes then
		return "-"
	end
	if bytes < 1024 then
		return string.format("%d B", bytes)
	elseif bytes < 1048576 then
		return string.format("%.1f KB", bytes / 1024)
	elseif bytes < 1073741824 then
		return string.format("%.1f MB", bytes / 1048576)
	else
		return string.format("%.1f GB", bytes / 1073741824)
	end
end

local function fmt_time(ts: number?): string
	if not ts then
		return "-"
	end
	return os.date("%Y-%m-%d %H:%M:%S", ts)
end

local function basename(path: string?): string
	if not path or path == "" then
		return "Yazi"
	end
	local name = path:match("([^/]+)/*$")
	return name or path
end

local function safe_cid(cid: any): string?
	if not cid then
		return nil
	end
	if type(cid) == "string" then
		return cid
	end
	if type(cid) == "number" then
		return string.format("%.0f", cid)
	end
	return tostring(cid)
end

local function join_path(dir: string?, name: string?): string
	if not dir or dir == "" then
		return name or ""
	end
	if not name or name == "" then
		return dir
	end
	if dir:sub(-1) == "/" then
		return dir .. name
	end
	return dir .. "/" .. name
end
local function file_ext(path: string): string
	local ext = path:match("%.([^./\\]+)$")
	return ext and ext:lower() or ""
end

local function detect_lang(ext: string): string
	local map = {
		lua = "lua",
		luau = "lua",
		py = "python",
		rs = "rust",
		js = "javascript",
		ts = "typescript",
		json = "json",
		toml = "toml",
		yaml = "yaml",
		yml = "yaml",
		md = "markdown",
		markdown = "markdown",
		sh = "bash",
		bash = "bash",
		zsh = "bash",
		c = "c",
		cpp = "cpp",
		h = "c",
		html = "html",
		css = "css",
		xml = "xml",
		sql = "sql",
		mermaid = "mermaid",
		mmd = "mermaid",
	}
	return map[ext] or "text"
end

local function mime_for(ext: string): string?
	local map = {
		png = "image/png",
		jpg = "image/jpeg",
		jpeg = "image/jpeg",
		gif = "image/gif",
		webp = "image/webp",
		svg = "image/svg+xml",
	}
	return map[ext]
end

local function is_archive(ext: string): boolean
	return ext == "zip" or ext == "tar" or ext == "gz" or ext == "tgz" or ext == "bz2" or ext == "xz" or ext == "7z" or ext == "rar" or ext == "zst"
end

-- Resolves semantic icon and tone aligned with Tern / Nerd Font file tree
local function file_icon(fname: string, is_dir: boolean?): (string, string)
	if is_dir == nil then return "?", "muted" end
	if is_dir then
		if fname:sub(1, 1) == "." then
			return "", "muted"
		end
		return "", "accent"
	end
	local ext = file_ext(fname)
	if is_archive(ext) then
		return "", "warn"
	elseif ext == "lua" or ext == "luau" then
		return "", "accent"
	elseif ext == "py" then
		return "", "warn"
	elseif ext == "rs" then
		return "", "error"
	elseif ext == "js" or ext == "ts" or ext == "jsx" or ext == "tsx" then
		return "", "warn"
	elseif ext == "sh" or ext == "bash" or ext == "zsh" then
		return "", "success"
	elseif ext == "c" or ext == "cpp" or ext == "h" or ext == "hpp" then
		return "", "accent"
	elseif ext == "go" then
		return "", "accent"
	elseif ext == "md" or ext == "markdown" then
		return "", "accent"
	elseif ext == "toml" or ext == "json" or ext == "yaml" or ext == "yml" then
		return "", "muted"
	elseif ext == "png" or ext == "jpg" or ext == "jpeg" or ext == "svg" or ext == "gif" or ext == "webp" then
		return "", "accent"
	elseif ext == "mp4" or ext == "mkv" or ext == "webm" or ext == "mp3" or ext == "wav" then
		return "", "accent"
	elseif ext == "pdf" then
		return "", "error"
	elseif ext == "txt" or ext == "log" then
		return "", "muted"
	end
	return "", "muted"
end

local function read_json(path: string): any?
	local ok, text = pcall(tern.fs.read, path, 256 * 1024)
	if not ok then return nil end
	local decoded, value = pcall(tern.json.decode, text)
	return decoded and type(value) == "table" and value or nil
end

-- Shell effects wake the owning window; window timers alone can sleep while idle.
tern.on("command_started", function(ev, cx) launch_contexts[ev.pane] = cx end)
tern.on("pane_exited", function(ev) launch_contexts[ev.pane] = nil end)

local function discover_launches()
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
			local cx = launch_contexts[health.owner_pane]
			if cx then
				-- A progressing supervisor, not an old snapshot, authorizes startup.
				cx:open(dir .. "/" .. fname)
				record.opened = true
			end
		end
		record.seq = health.seq
	end
	for cid in pairs(launches) do if not seen[cid] then launches[cid] = nil end end
end

local function poll_launches()
	local ok, err = pcall(discover_launches)
	if not ok then tern.log.warn("Yazi startup discovery failed", tostring(err)) end
	tern.timer(500, poll_launches)
end
tern.timer(500, poll_launches)

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
		state.trash = nil
		state.active_file = nil
		state.optimistic_cwd = nil
	end
	return live
end

-- Only the live, leased block may forward a native shell request to its window.
local function poll_native_shells(state: any, cx: any)
	if not state.token or not state.live or state.closing then return end
	local dir = state_dir()
	local ok, lease = pcall(tern.fs.read, dir .. "/managed-" .. state.cid .. ".lease", 4096)
	if not ok then return end
	local token, pane = lease:match("^(%S+)%s+(%d+)")
	if token ~= state.token or tonumber(pane) ~= cx.pane then return end
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
			or body.token ~= state.token or body.owner_pane ~= state.owner_pane or body.pane ~= cx.pane then continue end
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
			pcall(function() cx:open(path) end)
		end
	end
	for id in pairs(state.native_shells) do if not seen[id] then state.native_shells[id] = nil end end
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

local function clear_shell_handoffs(state: any)
	for id, handoff in pairs(state.shell_handoffs or {}) do
		if handoff.timer then handoff.timer:cancel() end
		pcall(tern.fs.remove, handoff.path)
		pcall(tern.fs.remove, handoff.path .. ".ack")
		state.shell_handoffs[id] = nil
	end
end

local function close_companion(state: any, cx: any)
	state.closing = true
	state.live = false
	state.requests = {}
	state.find_mode, state.find_query, state.find_previous = false, "", false
	clear_shell_handoffs(state)
	if state.timer then state.timer:cancel(); state.timer = nil end
	running[cx.pane] = nil
	stop_backend(state)
	cx:exit(0)
end

local function managed_poll(state: any, ev: any, cx: any)
	-- PaneInfo has no launch args. This token-bound nonce ACK proves the hinted
	-- pane is this client, even after a reload or pane-id reuse.
	if ev.cid ~= state.cid or ev.token ~= state.token or type(ev.nonce) ~= "string"
		or not ev.nonce:match("^[%w-]+$") or not refresh_live(state) then return end
	state.lease_nonce = ev.nonce
	local lease = string.format("%s %d %d %d %s\n", state.token, cx.pane,
		math.max(1, cx.rows), math.max(1, cx.cols), ev.nonce)
	pcall(tern.fs.write, state_dir() .. "/managed-" .. state.cid .. ".lease", lease)
end

local function fetch_archive_contents(full_url: string, ext: string, state: any, cx: any)
	local cache = state.archive_cache
	if cache[full_url] ~= nil then return end
	cache[full_url] = false
	local cmd
	if ext == "zip" then
		cmd = { "unzip", "-Z1", full_url }
	elseif ext == "tar" or ext == "gz" or ext == "tgz" or ext == "bz2" or ext == "xz" then
		cmd = { "tar", "-tf", full_url }
	elseif ext == "7z" then
		cmd = { "7z", "l", "-ba", "-slt", full_url }
	else
		cmd = { "tar", "-tf", full_url }
	end

	pcall(function()
		tern.process.run(cmd, function(res)
			if state.closing or state.archive_cache ~= cache or running[cx.pane] ~= state then return end
			if res and res.status == 0 and res.stdout then
				local lines = {}
				for line in res.stdout:gmatch("[^\r\n]+") do
					if #line > 0 then
						table.insert(lines, line)
						if #lines >= 50 then break end
					end
				end
				cache[full_url] = lines
			else
				cache[full_url] = {}
			end
			cx:render()
		end)
	end)
end
-- ---------------------------------------------------------------- rich preview builder
-- Plugin-scoped byte limits; bounded reads reject whole oversized files, not prefixes.
local PREVIEW_LIMITS = {
	image_bytes = { default = 16 * 1024 * 1024, max = 16 * 1024 * 1024 },
	markdown_bytes = { default = 1024 * 1024, max = 4 * 1024 * 1024 },
	code_bytes = { default = 1024 * 1024, max = 4 * 1024 * 1024 },
	mermaid_bytes = { default = 256 * 1024, max = 1024 * 1024 },
}

local function read_preview(path: string, kind: string): (string?, Node?)
	local spec = PREVIEW_LIMITS[kind]
	local ok_config, config = pcall(tern.kv.get, "preview_limits")
	if not ok_config then
		return nil, ui.text({ ui.span("Preview settings unavailable", "warn") })
	end
	local value = type(config) == "table" and config[kind] or nil
	local limit = spec.default
	if type(value) == "number" and value > 0 and value < math.huge and value % 1 == 0 then
		limit = math.min(value, spec.max)
	end
	local ok_read, content = pcall(tern.fs.read, path, limit)
	if not ok_read then
		return nil, ui.node("col", { role = "yazi.preview-unavailable", gap = "none" }, {
			ui.text({ ui.span("Preview unavailable (" .. fmt_size(limit) .. " limit)", "warn") }),
		})
	end
	return content, nil
end


-- Rich native file preview; directory rows come only from Yazi snapshots.
local function build_rich_preview(full_url: string, active_name: string, state: any, cx: any): Node
	local ext = file_ext(full_url)
	local img_mime = mime_for(ext)

	-- 0. Compressed Archive Preview (.zip, .tar, .tar.gz, .tgz, .7z, etc.)
	if is_archive(ext) then
		fetch_archive_contents(full_url, ext, state, cx)
		local cached = state.archive_cache and state.archive_cache[full_url]
		if type(cached) == "table" and #cached > 0 then
			local entry_nodes = {}
			for i = 1, math.min(18, #cached) do
				local item_name = cached[i]
				local is_sub_dir = (item_name:sub(-1) == "/")
				local sym, tone = file_icon(item_name, is_sub_dir)
				table.insert(entry_nodes, ui.text({
					ui.span(sym .. " ", tone),
					ui.span(item_name),
				}))
			end
			if #cached > 18 then
				table.insert(entry_nodes, ui.text({ ui.span(string.format("… and %d more files inside", #cached - 18), "muted") }))
			end
			return ui.col({
				ui.text({ ui.span(string.format("Archive Contents (%d items)", #cached), "warn") }),
				ui.rule(""),
				ui.col(entry_nodes),
			})
		elseif cached == false then
			return ui.text({ ui.span("Reading archive table of contents...", "warn") })
		else
			return ui.text({ ui.span("Archive empty or unreadable", "muted") })
		end
	end

	local kind = "code_bytes"
	if img_mime then
		kind = "image_bytes"
	elseif ext == "mermaid" or ext == "mmd" then
		kind = "mermaid_bytes"
	elseif ext == "md" or ext == "markdown" then
		kind = "markdown_bytes"
	end
	local content, read_error = read_preview(full_url, kind)
	if not content then
		return read_error
	end

	-- 1. SVG and Image Native Preview (via cx:blob + ui.node("image", ...))
	if img_mime then
		if #content == 0 then
			return ui.text({ ui.span("Image file is empty", "muted") })
		end
		local ok_blob, blob_id = pcall(function()
			return cx:blob(content, img_mime)
		end)
		if not ok_blob or not blob_id then
			return ui.text({ ui.span("Image preview unavailable", "warn") })
		end
		return ui.node("col", { role = "yazi.image-preview", gap = "sm" }, {
			ui.node("image", {
				blob = blob_id,
				alt = active_name,
				role = "yazi.image",
				max = { w = 1, h = tostring(math.max(1, (cx.rows or 24) - 12)) .. "lines" },
			}),
			ui.text({ ui.span(string.format("Image (%s, %s)", ext:upper(), fmt_size(#content)), "muted") }),
		})
	end

	-- 2. Mermaid diagram preview (.mermaid / .mmd)
	if ext == "mermaid" or ext == "mmd" then
		return ui.md("```mermaid\n" .. content .. "\n```")
	end

	-- 3. Markdown rich formatted document preview (.md / .markdown)
	if ext == "md" or ext == "markdown" then
		return ui.md(content)
	end

	-- 4. Code / Text syntax highlighting preview
	if content:find("\0") then
		return ui.col({
			ui.text({ ui.span("Binary file (preview suppressed)", "muted") }),
			ui.text({ ui.span("Size: " .. fmt_size(#content), "strong") }),
		})
	end
	return ui.code(content, detect_lang(ext))
end

-- Keep native selection reveal enabled; CSS fits the scroller to the column's
-- remaining height. List rows are taller than terminal cells, so never pad them.
local function viewport_list(items: { any }, max_lines: number, key: string, selected: number?): Node
	local node = ui.list(items)
	node.p = node.p or {}
	node.p.max = { lines = max_lines }
	node.p.key = key
	for i, row in ipairs(node.c or {}) do
		row.p.key = items[i].name
		row.p.title = items[i].url
	end
	if selected and items[selected] then
		node.p.selected = LIST_IDS[key] .. "." .. items[selected].name
	end
	return node
end

-- Native list events carry an item node id, not a numeric row index.
local function list_event_name(ev: any): string?
	if type(ev.id) ~= "string" or type(ev.item) ~= "string" then
		return nil
	end
	local prefix = ev.id .. "."
	if ev.item:sub(1, #prefix) == prefix then
		return ev.item:sub(#prefix + 1)
	end
	return nil
end

-- Optimistic input lasts until Yazi publishes a different cursor or directory.
-- Task-only pulses must not clear a click that is still being dispatched.
local function sync_hover(state: any, yz: YaziState?)
	if not yz then
		return
	end
	local source = safe_cid(yz.client_id)
	local hovered = yz.hovered and yz.hovered.url
	if state.source_client ~= source or state.source_cwd ~= yz.cwd then state.pending_g = nil end
	if state.source_client ~= source or state.source_cwd ~= yz.cwd or state.source_hover ~= hovered then
		state.active_file = nil
		state.optimistic_cwd = nil
		state.source_client = source
		state.source_cwd = yz.cwd
		state.source_hover = hovered
	end
	-- Keep local typing until all actor requests settle; then trust the snapshot.
	if not state.filter_mode and not state.sending and (not state.requests or #state.requests == 0)
		and yz.filter ~= nil and (state.filter_source ~= source or state.filter_seq ~= yz.seq) then
		state.filter_query = type(yz.filter) == "string" and yz.filter or ""
		state.filter_source, state.filter_seq = source, yz.seq
	end
end

-- Never enumerate paths to infer types. Each directory belongs to this snapshot.
local function compute_directory(yz: YaziState?, cwd: string): ParentInfo?
	if not yz then return nil end
	if yz.cwd == cwd then return yz end
	if yz.parent and yz.parent.cwd == cwd then return yz.parent end
	if yz.preview and yz.preview.cwd == cwd then return yz.preview end
	return nil
end

local function file_is_dir(yz: YaziState?, cwd: string, name: string): boolean?
	local folder = compute_directory(yz, cwd)
	local is_dir = folder and folder.file_dirs and folder.file_dirs[name]
	if type(is_dir) == "boolean" then return is_dir end
	if yz and yz.hovered and yz.hovered.url == join_path(cwd, name) then return yz.hovered.dir end
	return nil
end

local function compute_parent(yz: YaziState?, cur_cwd: string): ({ string }?, string?, { [string]: boolean }?)
	if cur_cwd == "/" then return nil, nil, nil end
	local parent_cwd = cur_cwd:match("(.+)/[^/]+$") or "/"
	local folder = compute_directory(yz, parent_cwd)
	return folder and folder.files, parent_cwd, folder and folder.file_dirs
end

-- Preserve all Yazi entries and ordering, including known optimistic directories.
local function compute_file_list(state: any, yz: YaziState, cur_cwd: string): { string }
	local folder = compute_directory(yz, cur_cwd)
	return folder and folder.files or {}
end

-- Persistent Yazi selection is distinct from the native cursor highlight.
local function selection_paths(yz: YaziState): { [string]: boolean }
	local paths = {}
	for _, path in ipairs(yz.selected_urls or {}) do
		paths[path] = true
	end
	return paths
end

local function selection_mark(paths: { [string]: boolean }, path: string): any
	return ui.span(paths[path] and "✓ " or "  ", paths[path] and "success" or "muted")
end

local request_seq = 0
local request_epoch = tostring(math.floor(tern.now() * 1000))
local pump

local function request(state: any, yz: any, cx: any, body: any, done: any?)
	local cid = safe_cid(yz and yz.client_id)
	if state.closing or (body.op ~= "ping" and not refresh_live(state)) then
		cx:toast("error", "Yazi is offline", "Reconnect before using file actions")
		return
	end
	if state.cid and cid ~= state.cid then cx:toast("error", "Yazi client mismatch", nil); return end
	if not cid then cx:toast("error", "No Yazi client", nil); return end
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
	body.id = prefix .. request_epoch .. "-" .. tostring(cx.pane) .. "-" .. tostring(request_seq)
	state.requests = state.requests or {}
	table.insert(state.requests, { cid = cid, body = body, done = done })
	pump(state, cx)
end

pump = function(state: any, cx: any)
	if state.sending or not state.requests or #state.requests == 0 then return end
	state.sending = true
	local job = state.requests[1]
	if state.closing or job.cid ~= state.cid or (job.body.op ~= "ping" and not refresh_live(state)) then
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
		if state.closing or running[cx.pane] ~= state then
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
				state.active_file = nil
				state.optimistic_cwd = nil
			end
			if body.op ~= "ping" or was_live then
				cx:toast("error", "Yazi request failed", reply.error or "No acknowledgement")
			end
		end
		if job.done then job.done(reply) end
		cx:render()
		pump(state, cx)
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

local function open_shell(state: any, cx: any, reply: any, query: string, block: boolean): boolean
	if state.closing or running[cx.pane] ~= state then return false end
	local shell = reply.shell
	if not block or type(reply.id) ~= "string" or not reply.id:match("^[%w-]+$") or type(shell) ~= "table"
		or type(shell.command) ~= "string" or type(shell.cwd) ~= "string" or shell.block ~= true then
		cx:toast("error", "Invalid shell launch", "Yazi did not prepare a visible terminal command")
		return false
	end
	local path = state_dir() .. "/shell-" .. reply.id .. ".json"
	state.shell_handoffs = state.shell_handoffs or {}
	local handoff = { path = path, query = query, block = block,
		shell = { command = shell.command, cwd = shell.cwd, block = shell.block } }
	state.shell_handoffs[reply.id] = handoff
	local ok, err = pcall(function()
		tern.fs.write(path, tern.json.encode({ version = 1, id = reply.id, cid = state.cid,
			token = state.token, pane = cx.pane, owner_pane = state.owner_pane, shell = shell }))
		cx:open(path)
	end)
	if not ok then
		state.shell_handoffs[reply.id] = nil
		pcall(tern.fs.remove, path)
		cx:toast("error", "Cannot open shell terminal", tostring(err))
		return false
	end
	handoff.timer = tern.timer(30000, function()
		handoff.timer = nil
		if state.shell_handoffs[reply.id] ~= handoff then return end
		state.shell_handoffs[reply.id] = nil
		pcall(tern.fs.remove, path)
		pcall(tern.fs.remove, path .. ".ack")
		if state.closing or running[cx.pane] ~= state then return end
		state.cmd_mode, state.cmd_block, state.cmd_query = true, handoff.block, handoff.query
		cx:toast("error", "Cannot open shell terminal", handoff.nonce
			and "Timed out waiting for terminal launch acknowledgement; launch outcome unknown"
			or "Timed out preparing visible terminal; command was not launched")
		cx:render()
	end)
	return true
end

local function shell_handoff_event(state: any, ev: any, cx: any)
	local handoff = state.shell_handoffs and state.shell_handoffs[ev.request_id]
	if state.closing or running[cx.pane] ~= state or not handoff or ev.cid ~= state.cid
		or ev.token ~= state.token or ev.pane ~= cx.pane or type(ev.nonce) ~= "string"
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
					cid = state.cid, token = state.token, pane = cx.pane, nonce = ev.nonce,
					shell = handoff.shell }))
				cx:open(handoff.path)
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
		state.cmd_mode, state.cmd_block, state.cmd_query = true, handoff.block, handoff.query
		cx:toast("error", "Cannot open shell terminal", ev.error or "Visible terminal launch failed")
	end
	cx:render()
end

local function open_selected(state: any, cx: any, selected: { string }?, ev: any?)
	if ev then
		local handoff = state.shell_handoffs and state.shell_handoffs[ev.request_id]
		local body = handoff and handoff.body
		if state.closing or running[cx.pane] ~= state or not body or ev.cid ~= body.cid
			or ev.token ~= body.token or ev.pane ~= body.pane or ev.owner_pane ~= body.owner_pane
			or state.cid ~= body.cid or state.token ~= body.token or cx.pane ~= body.pane
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
					cx:open(handoff.path)
				end)
				if ok then return end
				err = tostring(result)
			end
		end
		state.shell_handoffs[ev.request_id] = nil
		pcall(tern.fs.remove, handoff.path)
		pcall(tern.fs.remove, handoff.path .. ".ack")
		if err or ev.act ~= "files_opened" then
			cx:toast("error", "Cannot open selected files", err or "Native file open failed")
		end
		return
	end
	if state.closing or running[cx.pane] ~= state or not refresh_live(state) then
		cx:toast("error", "Yazi is offline", "Reconnect before using file actions")
		return
	end
	local cid = safe_cid(state.cid)
	if not cid then cx:toast("error", "No Yazi client", nil); return end
	-- Copy, never normalize or consult hover again: Alpha.txt and alpha.txt are
	-- distinct frozen targets even if the actor changes selection before ACK.
	local paths = {}
	for i, path in ipairs(selected or {}) do
		if type(path) ~= "string" or path:sub(1, 1) ~= "/" or path:find("%z") then
			cx:toast("error", "Cannot open selected files", "Yazi supplied an invalid selected path")
			return
		end
		paths[i] = path
	end
	if #paths == 0 then return end
	request_seq += 1
	local prefix = state.token and (cid .. "-" .. state.token .. "-") or ""
	local id = prefix .. request_epoch .. "-" .. tostring(cx.pane) .. "-" .. tostring(request_seq)
	local path = state_dir() .. "/files-" .. id .. ".json"
	local body = { version = 1, id = id, cid = cid, token = state.token,
		pane = cx.pane, owner_pane = state.owner_pane, paths = paths }
	-- Reuse the owned shell inbox lifecycle so close/reload removes both files.
	state.shell_handoffs = state.shell_handoffs or {}
	state.shell_handoffs[id] = { path = path, body = body }
	local ok, err = pcall(function()
		tern.fs.write(path, tern.json.encode(body))
		cx:open(path)
	end)
	if not ok then
		state.shell_handoffs[id] = nil
		pcall(tern.fs.remove, path)
		pcall(tern.fs.remove, path .. ".ack")
		cx:toast("error", "Cannot open selected files", tostring(err))
	end
end

local function action(state: any, yz: any, cx: any, name: string, args: any)
	request(state, yz, cx, { op = "action", action = name, args = args })
end

local function clear_filter(state: any, yz: any, cx: any)
	state.filter_mode = false
	state.filter_query = ""
	request(state, yz, cx, { op = "filter", query = "" })
end

local function clear_find(state: any, yz: any, cx: any)
	state.find_mode = false
	state.find_query = ""
	state.find_previous = false
	action(state, yz, cx, "escape", { find = true })
end

local function erase_codepoint(text: string): string
	if text == "" then return text end
	local i = #text
	while i > 1 and text:byte(i) >= 128 and text:byte(i) < 192 do i -= 1 end
	return text:sub(1, i - 1)
end

local function typed_text(key: any): string?
	if key.name == "paste" then return key.text end
	if key.ctrl or key.meta or key.alt then return nil end
	if key.text and key.text ~= "" then return key.text end
	if key.name == "space" then return " " end
	if #(key.name or "") == 1 then
		return key.shift and key.name:upper() or key.name
	end
	return nil
end

local function trash_decision(state: any, cx: any, confirm: boolean)
	local pending = state.trash
	state.trash = nil
	if confirm and pending then
		request(state, { client_id = pending.cid }, cx, { op = "trash_commit", paths = pending.paths }, function(reply)
			if reply.ok then cx:toast("info", "Trash queued in Yazi", "Completion is reported by Yazi tasks") end
		end)
	end
end

local handle_key

local PATH_FIELD = "main.browser.path.content.field"

local function utf16_size(text: string): number
	local size = 0
	for _, cp in utf8.codes(text) do size += cp > 0xFFFF and 2 or 1 end
	return size
end

local function byte_at_utf16(text: string, offset: number): number
	local size = 0
	for pos, cp in utf8.codes(text) do
		if size >= offset then return pos end
		size += cp > 0xFFFF and 2 or 1
	end
	return #text + 1
end

local function replace_path(state: any, text: string, first: number?, last: number?)
	local query = state.path_query or ""
	local cursor = state.path_cursor or utf16_size(query)
	local anchor = state.path_anchor or cursor
	first = first or math.min(cursor, anchor)
	last = last or math.max(cursor, anchor)
	state.path_query = query:sub(1, byte_at_utf16(query, first) - 1)
		.. text .. query:sub(byte_at_utf16(query, last))
	state.path_cursor = first + utf16_size(text)
	state.path_anchor = state.path_cursor
end

local function resolve_directory(query: string, cwd: string): string?
	if query == "" or query:find("[%z\r\n]") then return nil end
	local home = tern.getenv("HOME") or ""
	if query == "~" or query:sub(1, 2) == "~/" then
		if home == "" then return nil end
		query = home .. query:sub(2)
	end
	if query:sub(1, 1) ~= "/" then query = join_path(cwd, query) end
	-- Preserve '..' for the filesystem to resolve through symbolic links.
	return query
end

local function jump_directory(state: any, yz: any, cx: any, target: string)
	local query = state.path_query
	state.path_menu = false
	request(state, yz, cx, { op = "cd", target = target }, function(reply)
		if reply.ok then
			state.active_file = nil
			state.optimistic_cwd = nil
			if state.path_query == query then state.path_mode = false end
		end
	end)
end

local function submit_path(state: any, yz: any, cx: any)
	local target = resolve_directory(state.path_query or "", yz.cwd or "/")
	if not target then
		cx:toast("error", "Invalid directory", "Enter a directory path on one line")
		return
	end
	jump_directory(state, yz, cx, target)
end

local function path_segments(cwd: string): { any }
	local home = tern.getenv("HOME") or ""
	local at_home = home ~= "" and (cwd == home or cwd:sub(1, #home + 1) == home .. "/")
	local root = at_home and home or "/"
	local segments = { { label = at_home and "~" or "/", path = root } }
	local tail = at_home and cwd:sub(#home + 1) or cwd
	local path = root
	for name in tail:gmatch("[^/]+") do
		path = join_path(path, name)
		table.insert(segments, { label = name, path = path })
	end
	return segments
end

local function path_header(state: any, cwd: string, cx: any): (any, any)
	local children, layer = {}, nil
	if state.path_mode then
		table.insert(children, ui.node("input", { key = "field", role = "yazi.path-input",
			text = state.path_query or "", cursor = state.path_cursor, anchor = state.path_anchor,
			placeholder = "Directory path", sendable = true, grow = 1, min = { w = 0 } }))
		table.insert(children, clickable(ui.el("button", { key = "go" }, { ui.text("Go") }), "path_submit"))
		table.insert(children, clickable(ui.el("button", { key = "cancel" }, { ui.text("Cancel") }), "path_cancel"))
	else
		local segments = path_segments(cwd)
		local budget = math.max(8, (cx.cols or 80) - 12)
		local cap = math.max(4, math.min(24, math.floor(budget / 2)))
		local first, used = #segments, math.min(cap, utf8.len(segments[#segments].label) or cap) + 5
		while first > 2 do
			local cost = math.min(cap, utf8.len(segments[first - 1].label) or cap) + 3
			if used + cost > budget then break end
			used += cost
			first -= 1
		end
		local function crumb(segment, index)
			return clickable(ui.el("button", { key = "dir" .. index, role = "yazi.path-segment",
				title = segment.path, min = { w = 0 }, max = { w = tostring(cap) .. "ch" } }, {
				ui.node("text", { text = segment.label, wrap = "none", truncate = "end" }),
			}), "path_cd:" .. segment.path)
		end
		table.insert(children, crumb(segments[1], 1))
		if first > 2 then
			table.insert(children, clickable(ui.el("button", { key = "more", role = "yazi.path-more",
				title = "Show ancestor directories" }, { ui.text("…") }), "path_more"))
		end
		for index = math.max(2, first), #segments do
			table.insert(children, ui.node("text", { key = "sep" .. index, text = "›", tone = "muted", shrink = 0 }))
			table.insert(children, crumb(segments[index], index))
		end
		-- The tail remains a distinct target even when every segment is truncated.
		table.insert(children, clickable(ui.el("button", { key = "edit", role = "yazi.path-edit",
			title = "Edit directory path", grow = 1, shrink = 0, min = { w = "3ch" } }, { ui.text(" ") }), "path_edit"))
		if state.path_menu then
			local ancestors = {}
			for index = 2, first - 1 do table.insert(ancestors, crumb(segments[index], index)) end
			table.insert(ancestors, clickable(ui.el("button", { key = "cancel" }, { ui.text("Cancel [Esc]") }), "path_cancel"))
			layer = ui.col({ ui.node("card", { key = "ancestors", role = "yazi.path-ancestors" }, {
				ui.text("Ancestor directories"), ui.col(ancestors),
			}) })
		end
	end
	return ui.node("card", { key = "path", role = "yazi.path-header", title = cwd }, {
		ui.node("row", { key = "content", role = "yazi.path-content", gap = "xs" }, children),
	}), layer
end

-- ---------------------------------------------------------------- Miller columns builders

-- Left Column: Parent directory as a scrollable list; the current
-- directory row is the list selection (native active capsule).
local function build_parent_column(yz: YaziState?, cur_cwd: string, max_lines: number, selected_paths: { [string]: boolean }): Node
	local parent_files, parent_cwd, parent_dirs = compute_parent(yz, cur_cwd)
	if not parent_files then
		return ui.card(
			{ ui.span("Parent", "muted") },
			{ ui.text({ ui.span(cur_cwd == "/" and "(root)" or "Waiting for Yazi directory metadata...", "muted") }) }
		)
	end

	local cur_dirname = basename(cur_cwd)
	local items = {}
	local selected = nil
	for _, fname in ipairs(parent_files) do
		local is_cur = (fname == cur_dirname)
		if is_cur then
			selected = #items + 1
		end
		local is_dir = parent_dirs and parent_dirs[fname]
		local sym, tone = file_icon(fname, is_dir)
		table.insert(items, {
			name = fname,
			url = join_path(parent_cwd, fname),
			label = { selection_mark(selected_paths, join_path(parent_cwd, fname)), ui.span(sym .. " ", tone), ui.span(fname, is_cur and "strong" or nil) },
		})
	end

	local parent_title = (parent_cwd and basename(parent_cwd) ~= "") and basename(parent_cwd) or "/"
	local card = ui.card(
		{ ui.span(parent_title, "muted") },
		{ viewport_list(items, max_lines, "parent_list", selected) }
	)
	card.p.title = parent_cwd or "/"
	return card
end

-- Right Column child preview: Expand subfolder contents as a scrollable list
local function render_child_directory_preview(yz: YaziState, full_url: string, dir_name: string, max_lines: number, selected_paths: { [string]: boolean }): Node
	local folder = compute_directory(yz, full_url)
	local list = folder and folder.files
	if not list then
		return ui.card(
			{ ui.span("[" .. dir_name .. "]", "accent") },
			{ ui.text({ ui.span("Waiting for Yazi directory preview...", "muted") }) }
		)
	end
	if #list == 0 then
		return ui.card(
			{ ui.span("[" .. dir_name .. "]", "accent") },
			{ ui.text({ ui.span("(empty directory)", "muted") }) }
		)
	end
	local child_items = {}
	for _, cname in ipairs(list) do
		local sub_path = join_path(full_url, cname)
		local sym, tone = file_icon(cname, folder.file_dirs and folder.file_dirs[cname])
		table.insert(child_items, {
			name = cname,
			url = sub_path,
			label = { selection_mark(selected_paths, sub_path), ui.span(sym .. " ", tone), cname },
		})
	end
	return ui.card(
		{ ui.span("[" .. dir_name .. "]", "accent") },
		{ viewport_list(child_items, max_lines, "child_list", 1) }
	)
end

-- ---------------------------------------------------------------- block definition

handle_key = function(state: any, key: any, cx: any): boolean
	local raw = key.name or ""
	local k = raw:lower()
	local yz = read_latest_state(state)
	sync_hover(state, yz)
	local escape = k == "escape" or k == "esc" or (key.ctrl and k == "[")
	local enter = k == "enter" or k == "return"
	local plain_g = k == "g" and not key.shift and raw == k and not key.ctrl and not key.alt and not key.meta
	local pending_g = state.pending_g == true
	state.pending_g = nil
	if not state.live then
		if (k == "q" or escape) and not key.ctrl and not key.alt and not key.meta then
			close_companion(state, cx)
		else cx:toast("error", "Yazi is offline", "Reconnect before using file actions") end
		return true
	end
	if state.trash then
		if escape then trash_decision(state, cx, false)
		elseif enter then trash_decision(state, cx, true) end
		return true
	end
	if state.path_menu then
		if escape then state.path_menu = false end
		return true
	end
	if state.path_mode then
		if escape then state.path_mode = false
		elseif enter then submit_path(state, yz, cx)
		elseif key.ctrl and k == "a" then
			state.path_anchor = 0; state.path_cursor = utf16_size(state.path_query or "")
		elseif k == "backspace" or k == "delete" then
			local query = state.path_query or ""
			local cursor = state.path_cursor or utf16_size(query)
			local anchor = state.path_anchor or cursor
			if cursor ~= anchor then replace_path(state, "")
			elseif k == "backspace" and cursor > 0 then
				local prefix = query:sub(1, byte_at_utf16(query, cursor) - 1)
				replace_path(state, "", utf16_size(erase_codepoint(prefix)), cursor)
			elseif k == "delete" then
				local next_byte = utf8.offset(query, 2, byte_at_utf16(query, cursor)) or (#query + 1)
				replace_path(state, "", cursor, utf16_size(query:sub(1, next_byte - 1)))
			end
		elseif k == "home" then state.path_cursor = 0; state.path_anchor = 0
		elseif k == "end" then
			state.path_cursor = utf16_size(state.path_query or ""); state.path_anchor = state.path_cursor
		else
			local text = typed_text(key)
			if text then replace_path(state, text) else return false end
		end
		return true
	end
	if state.cmd_mode then
		if escape then state.cmd_mode = false; state.cmd_query = ""
		elseif enter then
			local query, block = state.cmd_query or "", state.cmd_block == true
			if query == "" then state.cmd_mode = false; return true end
			request(state, yz, cx, { op = "command", action = "shell", args = { query, block = block } }, function(reply)
				if not reply.ok then return end
				if block and not state.token then
					if not open_shell(state, cx, reply, query, block) then return end
				else
					cx:toast("info", "Shell queued in Yazi", "Task accepted; completion is reported by Yazi tasks")
				end
				if state.cmd_query == query and state.cmd_block == block then
					state.cmd_mode = false; state.cmd_query = ""
				end
			end)
		elseif k == "backspace" then state.cmd_query = erase_codepoint(state.cmd_query or "")
		else
			local text = typed_text(key)
			if text then state.cmd_query = (state.cmd_query or "") .. text end
		end
		return true
	end
	if state.find_mode then
		if escape then clear_find(state, yz, cx)
		elseif enter then
			local query, previous = state.find_query or "", state.find_previous == true
			state.find_mode = false
			state.find_query = ""
			state.active_file = nil
			request(state, yz, cx, { op = "find", query = query, previous = previous })
		else
			local query = state.find_query or ""
			if k == "backspace" then query = erase_codepoint(query)
			else local text = typed_text(key); if not text then return true end; query ..= text end
			state.find_query = query
			state.active_file = nil
			request(state, yz, cx, { op = "find", query = query, previous = state.find_previous == true })
		end
		return true
	end
	if state.filter_mode then
		if escape then clear_filter(state, yz, cx)
		elseif enter then
			state.filter_mode = false
			request(state, yz, cx, { op = "filter", query = state.filter_query or "", done = true })
		else
			local query = state.filter_query or ""
			if k == "backspace" then query = erase_codepoint(query)
			else local text = typed_text(key); if not text then return true end; query ..= text end
			state.filter_query = query
			state.active_file = nil
			request(state, yz, cx, { op = "filter", query = query, done = false })
		end
		return true
	end
	if key.alt or key.meta then return false end
	if escape then
		state.filter_mode = false
		action(state, yz, cx, "escape", {})
		return true
	end
	if key.ctrl then
		if k == "a" then action(state, yz, cx, "toggle_all", { state = "on" }); return true end
		if k == "r" then action(state, yz, cx, "toggle_all", {}); return true end
		if k ~= "d" and k ~= "u" and k ~= "f" and k ~= "b" then return false end
	end
	local upper = key.shift or raw ~= k
	local page_down = k == "page_down" or k == "pagedown" or (key.ctrl and (k == "d" or k == "f"))
	local page_up = k == "page_up" or k == "pageup" or (key.ctrl and (k == "u" or k == "b"))
	if page_down or page_up then
		local half = (key.ctrl and (k == "d" or k == "u")) or (not key.ctrl and key.shift)
		action(state, yz, cx, "arrow", { (page_up and "-" or "") .. (half and "50%" or "100%") })
		return true
	end
	if key.ctrl then return false end
	if k == "end" or (k == "g" and upper) then action(state, yz, cx, "arrow", { "bot" }); return true end
	if k == "home" then action(state, yz, cx, "arrow", { "top" }); return true end
	if plain_g then
		if pending_g then action(state, yz, cx, "arrow", { "top" })
		else state.pending_g = true end
		return true
	end
	if k == "n" then action(state, yz, cx, "find_arrow", { previous = upper }); return true end
	if upper and k:match("^[a-z]$") then
		if k == "v" then action(state, yz, cx, "visual_mode", { unset = true })
		elseif k == "h" then action(state, yz, cx, "back", {})
		elseif k == "l" then action(state, yz, cx, "forward", {})
		else return false end
		return true
	end
	if k == "j" or k == "down" or k == "arrowdown" or k == "arrow_down" or k == "k" or k == "up" or k == "arrowup" or k == "arrow_up" then
		local step = (k == "j" or k == "down" or k == "arrowdown" or k == "arrow_down") and 1 or -1
		action(state, yz, cx, "arrow", { tostring(step) })
	elseif k == "h" or k == "left" or k == "arrowleft" or k == "arrow_left" then
		action(state, yz, cx, "leave", {})
	elseif k == "l" or k == "right" or k == "arrowright" or k == "arrow_right" then
		action(state, yz, cx, "enter", {})
	elseif enter or k == "o" then
		local selected = yz and yz.selected_urls or {}
		if #selected > 0 then
			open_selected(state, cx, selected)
		elseif yz and yz.hovered and yz.hovered.url then
			if enter and yz.hovered.dir == true then action(state, yz, cx, "enter", {})
			else cx:open(yz.hovered.url) end
		end
	elseif k == "space" or k == " " then request(state, yz, cx, { op = "toggle_advance" })
	elseif k == "v" then action(state, yz, cx, "visual_mode", {})
	elseif k == "y" then action(state, yz, cx, "yank", {})
	elseif k == "x" then action(state, yz, cx, "yank", { cut = true })
	elseif k == "d" then
		request(state, yz, cx, { op = "trash_prepare" }, function(reply)
			if reply.ok and reply.paths and #reply.paths > 0 then
				state.trash = { cid = safe_cid(yz.client_id), paths = reply.paths }
			elseif reply.ok then cx:toast("info", "Nothing to trash", nil) end
		end)
	elseif k == "f" then state.filter_mode = true
	elseif k == "/" or k == "?" then
		state.find_mode = true
		state.find_query = ""
		state.find_previous = k == "?" or key.shift == true
	elseif (k == ";" or k == "semicolon") and not key.shift then state.cmd_mode = true; state.cmd_block = false; state.cmd_query = ""
	elseif k == ":" or k == "colon" or ((k == ";" or k == "semicolon") and key.shift) then state.cmd_mode = true; state.cmd_block = true; state.cmd_query = ""
	elseif k == "." or k == "period" then action(state, yz, cx, "hidden", { state = "toggle" })
	elseif k == "q" then close_companion(state, cx)
	else return false end
	return true
end

-- Host timers keep idle daemon-owned panes alive independently of window frames.
local function poll_block(state)
	state.timer = tern.timer(500, function()
		local cx = state.cx
		state.timer = nil
		if state.closing or running[cx.pane] ~= state then return end
		local pane_alive, owner_alive = false, state.owner_pane == nil
		for _, info in ipairs(tern.pane.list()) do
			if info.pane == cx.pane then pane_alive = true end
			if info.pane == state.owner_pane then owner_alive = true end
		end
		if not pane_alive or not owner_alive then
			state.closing = true
			clear_shell_handoffs(state)
			running[cx.pane] = nil
			stop_backend(state)
			return
		end
		local yz = read_latest_state(state)
		poll_native_shells(state, cx)
		if state.token and state.lease_nonce then
			managed_poll(state, { cid = state.cid, token = state.token, nonce = state.lease_nonce }, cx)
		elseif not state.token and yz and not state.sending and tern.now() >= (state.next_probe or 0) then
			state.next_probe = tern.now() + 2000
			request(state, yz, cx, { op = "ping" })
		end
		local seq = yz and (yz.ts or yz.seq)
		local config_ok, config = pcall(function() return tern.json.encode(tern.kv.get("preview_limits")) end)
		local config_stamp = tostring(config_ok) .. tostring(config)
		if seq ~= state.render_seq or state.live ~= state.render_live or config_stamp ~= state.render_config then
			state.render_seq, state.render_live, state.render_config = seq, state.live, config_stamp
			cx:render()
		end
		poll_block(state)
	end)
end

tern.block.define("companion", {
	features = { "edit" },
	init = function(cx, args, saved)
		args = args or {}
		saved = type(saved) == "table" and saved or {}
		local cid = safe_cid(args[1] or saved.cid)
		if cid and not cid:match("^%d+$") then cid = nil end
		local token = args[2] or saved.token
		if type(token) ~= "string" or not token:match("^[%w-]+$") then token = nil end
		local nonce = args[5] or saved.lease_nonce
		if type(nonce) ~= "string" or not nonce:match("^[%w-]+$") then nonce = nil end
		local previous = running[cx.pane]
		if previous and previous.timer then previous.timer:cancel() end
		if previous then previous.closing = true; clear_shell_handoffs(previous) end
		local state = {
			cx = cx,
			cid = cid,
			token = token,
			lease_nonce = nonce,
			owner_pane = tonumber(args[3]) or saved.owner_pane,
			source_cwd = args[4] or saved.source_cwd,
			live = false,
			active_file = nil,
			optimistic_cwd = nil,
			filter_mode = false,
			filter_query = "",
			find_mode = false,
			find_query = "",
			find_previous = false,
			cmd_mode = false,
			cmd_query = "",
			cmd_block = false,
			archive_cache = {},
		}
		running[cx.pane] = state
		poll_block(state)
		return state
	end,

	view = function(state, cx)
		state.cx = cx
		local yz = read_latest_state(state)
		if not yz or not state.live then
			return {
				main = ui.card(
					{ ui.span(yz and "Yazi is offline" or "Connect to Yazi", "warn") },
					{
						ui.text(state.token and "The managed backend stopped or is not responding. File actions are disabled."
							or "Explicit attach requires a running Yazi with the tern.yazi plugin; checking actor acknowledgement."),
						ui.text(state.cid and ("Client " .. state.cid) or "No Yazi snapshot is available."),
						ui.text("Press q or Esc to close this panel."),
					}
				),
				dock = ui.row({
					clickable(ui.el("button", {}, { ui.text("Check Again") }), "poll"),
				}),
			}
		end
		sync_hover(state, yz)
		-- Async results never outlive the directory/snapshot that requested them.
		local preview_stamp = tostring(yz.client_id) .. ":" .. tostring(yz.seq) .. ":" .. yz.cwd
		if state.preview_stamp ~= preview_stamp then
			state.preview_stamp = preview_stamp
			state.archive_cache = {}
		end

		local cur_cwd = state.optimistic_cwd or yz.cwd or "/"
		if state.optimistic_cwd and yz.cwd == state.optimistic_cwd then
			state.optimistic_cwd = nil
		end

		local top_rows = {}
		local header, path_layer = path_header(state, cur_cwd, cx)
		table.insert(top_rows, header)

		-- Keep one input line reserved so editors do not shift the file columns.
		local input_bar = ui.row({ ui.text(" ") })
		if state.cmd_mode then
			input_bar = ui.row({
				ui.badge(state.cmd_block and "Shell [:]" or "Shell [;]", "warn"),
				ui.text({
					ui.span(" " .. (state.cmd_block and ":" or ";") .. (state.cmd_query or ""), "strong"),
					ui.span(state.cmd_mode and " ▎" or "", "warn"),
				}),
				clickable(ui.badge("Cancel [Esc]", "muted"), "clear_cmd"),
			})
		elseif state.find_mode then
			input_bar = ui.row({
				ui.badge(state.find_previous and "Find [?]" or "Find [/]", "accent"),
				ui.text({ ui.span(" " .. (state.find_query or ""), "strong"), ui.span(" ▎", "accent") }),
				clickable(ui.badge("Cancel [Esc]", "muted"), "clear_find"),
			})
		elseif state.filter_mode then
			input_bar = ui.row({
				ui.badge("Filter [f]", "accent"),
				ui.text({
					ui.span(" " .. (state.filter_query or ""), "strong"),
					ui.span(state.filter_mode and " ▎" or "", "accent"),
				}),
				clickable(ui.badge("Clear [Esc]", "muted"), "clear_filter"),
			})
		end
		input_bar.p = input_bar.p or {}
		input_bar.p.key = "input"
		input_bar.p.role = "yazi.input-row"
		input_bar.p.min = { h = "1.5lines" }
		input_bar.p.max = { h = "1.5lines" }
		table.insert(top_rows, input_bar)

		-- 2. Task progress (if background tasks are running)
		if yz.tasks and yz.tasks.total and yz.tasks.total > 0 then
			local t = yz.tasks
			local done = (t.succ or 0) + (t.fail or 0)
			local running = t.total - done
			if running > 0 then
				local ratio = t.total > 0 and (done / t.total) or 0
				local task_card = ui.card(
					{ ui.span(string.format("Tasks: %d active / %d total", running, t.total), "warn") },
					{ ui.progress(ratio, string.format("%d of %d done", done, t.total)) }
				)
				table.insert(top_rows, task_card)
			end
		end

		-- 3. File List (scrollable viewport; the list widget owns scrolling)
		local file_list = compute_file_list(state, yz, cur_cwd)
		local selected_paths = selection_paths(yz)
		local marked_paths = {}
		for _, path in ipairs(yz.marked_urls or {}) do marked_paths[path] = true end

		local hovered_url = yz.cwd == cur_cwd and yz.hovered and yz.hovered.url
		local hovered_name = hovered_url and basename(hovered_url) or nil
		local active_name = state.active_file or hovered_name or file_list[1]

		local cur_idx = 1
		for idx, fname in ipairs(file_list) do
			if fname == active_name then
				cur_idx = idx
				break
			end
		end

		-- This is only the native list ceiling, not a terminal-to-list row conversion.
		-- The columns and their scrollers take the actual remaining pane height.
		local col_lines = math.max(1, cx.rows or 24)

		-- Determine active item type & paths
		local active_full_url = active_name and join_path(cur_cwd, active_name) or cur_cwd
		local is_active_dir = active_name and file_is_dir(yz, cur_cwd, active_name)

		-- ---------------- Left Column (Parent Directory)
		local left_col_card = build_parent_column(yz, cur_cwd, col_lines, selected_paths)

		-- ---------------- Middle Column (Current Directory, scrollable)
		local middle_items = {}
		for _, fname in ipairs(file_list) do
			local is_active = (fname == active_name)
			local item_path = join_path(cur_cwd, fname)
			local sym, tone = file_icon(fname, file_is_dir(yz, cur_cwd, fname))
			table.insert(middle_items, {
				name = fname,
				url = item_path,
				label = { selection_mark(selected_paths, item_path), ui.span(marked_paths[item_path] and (yz.mode == "unset" and "- " or "+ ") or "", "warn"), ui.span(sym .. " ", tone), fname },
				tone = is_active and "accent" or nil,
			})
		end

		local middle_title = string.format("%s (%d/%d)", basename(cur_cwd), cur_idx, #file_list)
		local middle_col_card = ui.card(
			{ ui.span(middle_title, "strong") },
			{ #middle_items > 0 and viewport_list(middle_items, col_lines, "files_list", cur_idx)
				or ui.text({ ui.span(cur_cwd == yz.cwd and "(empty directory)" or "Waiting for Yazi directory metadata...", "muted") }) }
		)

		-- ---------------- Right Column (Preview / Child Subfolder Expansion)
		local right_col_card
		if not active_name then
			right_col_card = ui.card(
				{ ui.span("Preview", "muted") },
				{ ui.text({ ui.span(cur_cwd == yz.cwd and "(no file selected)" or "Waiting for Yazi directory metadata...", "muted") }) }
			)
		elseif is_active_dir then
			-- Child Directory Content Expansion (exactly like Yazi right pane!)
			right_col_card = render_child_directory_preview(yz, active_full_url, active_name, col_lines, selected_paths)
		elseif is_active_dir == nil then
			right_col_card = ui.card({ ui.span(active_name, "strong") }, {
				ui.text({ ui.span("Waiting for Yazi file metadata...", "muted") }),
			})
		else
			-- Rich File Preview (Code, Markdown, Image, Archive table of contents)
			local preview_content = build_rich_preview(active_full_url, active_name, state, cx)
			local act_buttons = {
				clickable(ui.el("button", {}, { ui.text("Open in Tern") }), "open_file=" .. active_full_url),
			}
			table.insert(act_buttons, clickable(ui.el("button", {}, { ui.text("Copy Path") }), "copy_path=" .. active_full_url))
			table.insert(act_buttons, clickable(ui.el("button", {}, { ui.text("Terminal Here") }), "terminal_here=" .. cur_cwd))

			right_col_card = ui.card(
				{ ui.span(active_name, "strong") },
				{
					ui.node("col", { role = "yazi.preview", gap = "none" }, { preview_content }),
					ui.node("row", { role = "yazi.preview-actions", gap = "xs", wrap = true }, act_buttons),
				}
			)
		end

		-- Content must not participate in column width allocation.
		left_col_card.p.role = "yazi.column.parent"
		left_col_card.p.key = "parent"
		middle_col_card.p.role = "yazi.column.current"
		middle_col_card.p.key = "current"
		middle_col_card.p.title = middle_title .. "\n" .. cur_cwd
		right_col_card.p.role = "yazi.column.preview"
		right_col_card.p.key = "preview"
		right_col_card.p.title = active_name and active_full_url or "Preview"
		local miller_columns = ui.node("row", { role = "yazi.columns", key = "columns" }, {
			left_col_card,
			middle_col_card,
			right_col_card,
		})
		table.insert(top_rows, miller_columns)

		-- ---------------- Bottom Status Bar (Yazi-style Pill Status Bar)
		local mode_str = state.cmd_mode and "CMD" or (state.find_mode and "FIND" or (state.filter_mode and "FILTER" or (yz.mode == "select" and "VIS" or (yz.mode == "unset" and "UNSET" or "NOR"))))
		local mode_tone = (state.cmd_mode or (yz.mode and yz.mode ~= "normal")) and "warn" or "accent"

		local pct_num = (#file_list > 0) and math.floor((cur_idx / #file_list) * 100) or 0
		local pct_str = string.format("%d%%", pct_num)
		local pos_str = string.format("%d/%d", cur_idx, math.max(1, #file_list))

		local status_bar = ui.node("row", { role = "yazi.status", gap = "xs" }, {
			ui.badge(mode_str, mode_tone),
			ui.badge(string.format("%d sel", yz.selected or 0), (yz.selected or 0) > 0 and "warn" or "muted"),
			ui.node("text", { role = "yazi.status-name", spans = { ui.span(active_name or "", "strong") } }),
			ui.text({ ui.span(" " .. pct_str .. " ", "muted") }),
			ui.badge(pos_str, "accent"),
		})
		local trash_layer = nil
		if state.trash then
			local rows = {}
			for i, path in ipairs(state.trash.paths) do rows[i] = { name = tostring(i), label = tern.json.encode(path) } end
			local list = ui.list(rows)
			list.p = list.p or {}
			list.p.key = "paths"
			list.p.max = { lines = math.max(4, (cx.rows or 24) - 8) }
			trash_layer = ui.col({ ui.node("card", { key = "trash", role = "yazi.trash-confirm" }, {
				ui.text("Move these exact paths to Trash? Cancel changes nothing."), list,
				ui.node("row", { key = "controls" }, {
					clickable(ui.el("button", { key = "cancel" }, { ui.text("Cancel [Esc]") }), "trash_cancel"),
					clickable(ui.el("button", { key = "confirm", tone = "warn" }, { ui.text("Move to Trash [Enter]") }), "trash_confirm"),
				}),
			}) })
		end

		return {
			main = ui.col({ ui.node("col", { role = "yazi.browser", key = "browser", gap = "sm" }, top_rows) }),
			-- Region roots are stripped; preserve the row as a child of the dock.
			dock = ui.col({ status_bar }),
			layer = trash_layer or path_layer,
		}
	end,

	title = function(state)
		local yz = read_latest_state(state)
		if yz and yz.cwd then
			return "Companion (" .. basename(yz.cwd) .. ")"
		end
		return "Yazi Companion"
	end,

	event = function(state, ev, cx)
		local act = ev.act
		local val = ev.value
		local yz = read_latest_state(state)
		sync_hover(state, yz)
		local cid = safe_cid(yz and yz.client_id)
		local cwd = yz and yz.cwd or ""
		if act == "shortcut" and type(ev.key) == "table" then handle_key(state, ev.key, cx); return end
		if act == "managed_poll" then managed_poll(state, ev, cx); return end
		if act == "shell_handoff_check" or act == "shell_launched" or act == "shell_failed" then
			shell_handoff_event(state, ev, cx)
			return
		end
		if act == "files_handoff_check" or act == "files_opened" or act == "files_failed" then
			open_selected(state, cx, nil, ev)
			return
		end
		if act == "poll" then
			if not state.token and yz and not state.sending and tern.now() >= (state.next_probe or 0) then
				state.next_probe = tern.now() + 2000
				request(state, yz, cx, { op = "ping" })
			end
			return
		end
		if not state.live then
			cx:toast("error", "Yazi is offline", "Reconnect before using file actions")
			return
		end
		if state.trash then
			if act == "trash_confirm" then trash_decision(state, cx, true)
			elseif act == "trash_cancel" then trash_decision(state, cx, false) end
			return
		end
		state.pending_g = nil
		if act == "path_edit" then
			state.path_mode, state.path_menu = true, false
			state.cmd_mode, state.find_mode, state.filter_mode = false, false, false
			state.path_query = cwd
			state.path_cursor, state.path_anchor = utf16_size(cwd), 0
			return
		elseif act == "path_cancel" then
			state.path_mode, state.path_menu = false, false
			return
		elseif act == "path_more" then
			state.path_menu = not state.path_menu
			return
		elseif act == "path_submit" then
			if state.path_mode then submit_path(state, yz, cx) end
			return
		elseif type(act) == "string" and act:sub(1, 8) == "path_cd:" then
			jump_directory(state, yz, cx, act:sub(9))
			return
		elseif ev.id == PATH_FIELD and state.path_mode then
			if ev.ev == "focus" then
				-- Select-all belongs only to entering path mode. Native pointer
				-- editing owns the caret after the user clicks inside the field.
				state.path_cursor, state.path_anchor = nil, nil
				return
			end
			if ev.ev == "edit" then
				if ev.len ~= utf16_size(state.path_query or "") then return end
				replace_path(state, ev.text or "", ev.from, ev.to)
				state.path_cursor, state.path_anchor = ev.cursor, ev.cursor
			elseif ev.ev == "send" then
				state.path_query = ev.text or state.path_query
				submit_path(state, yz, cx)
			end
			return
		end

		if ev.ev == "select" or ev.ev == "activate" then
			local name = list_event_name(ev)
			local cur_cwd_ev = state.optimistic_cwd or cwd
			local base, names, dirs
			if ev.id == LIST_IDS.parent_list then
				names, base, dirs = compute_parent(yz, cur_cwd_ev)
			elseif ev.id == LIST_IDS.files_list and yz then
				base = cur_cwd_ev
				names = compute_file_list(state, yz, cur_cwd_ev)
				local folder = compute_directory(yz, base)
				dirs = folder and folder.file_dirs
			elseif ev.id == LIST_IDS.child_list then
				local active = state.active_file
					or (yz and yz.hovered and yz.hovered.url and basename(yz.hovered.url))
				if active then
					base = join_path(cur_cwd_ev, active)
					local folder = compute_directory(yz, base)
					names = folder and folder.files
					dirs = folder and folder.file_dirs
				end
			end
			if not name or not base or not names or not table.find(names, name) then
				return
			end

			local target = join_path(base, name)
			local is_dir = dirs and dirs[name]
			if type(is_dir) ~= "boolean" then is_dir = file_is_dir(yz, base, name) end
			if ev.ev == "activate" and is_dir == nil then return end
			if ev.ev == "activate" and is_dir then
				state.optimistic_cwd = target
				state.active_file = nil
				request(state, yz, cx, { op = "activate", target = target, action = "enter" })
			else
				if base ~= cwd then
					state.optimistic_cwd = base
				end
				state.active_file = name
				if ev.ev == "activate" then
					cx:open(target)
				else action(state, yz, cx, "reveal", { target, no_dummy = true }) end
			end
			return
		end

		if act == "copy_path" and val then
			cx:copy(val)
			cx:toast("success", "Path copied to clipboard", val)
		elseif act == "reveal" and val then
			state.active_file = val
			action(state, yz, cx, "reveal", { join_path(state.optimistic_cwd or cwd, val), no_dummy = true })
		elseif act == "enter_dir" and val then
			state.optimistic_cwd = val
			state.active_file = nil
			request(state, yz, cx, { op = "activate", target = val, action = "enter" })
		elseif act == "open_file" and val then
			cx:open(val)
		elseif act == "terminal_here" and val then
			local ok, err = pcall(function()
				tern.process.run({ "tern", "split", tostring(cx.pane), "down", "--cwd", val }, function(res)
					if state.closing or running[cx.pane] ~= state then return end
					if not res or res.status ~= 0 then
						cx:toast("error", "Cannot open terminal", res and res.stderr or "Missing launch result")
					end
				end)
			end)
			if not ok then cx:toast("error", "Cannot open terminal", tostring(err)) end
		elseif act == "leave" then
			action(state, yz, cx, "leave", {})
		elseif act == "clear_filter" then
			clear_filter(state, yz, cx)
		elseif act == "clear_find" then
			clear_find(state, yz, cx)
		elseif act == "clear_cmd" then
			state.cmd_mode = false
			state.cmd_query = ""
		end
	end,

	key = function(state, key, cx)
		return handle_key(state, key, cx)
	end,

	save = function(state)
		return { cid = state.cid, token = state.token, owner_pane = state.owner_pane,
			source_cwd = state.source_cwd, lease_nonce = state.lease_nonce }
	end,
})
