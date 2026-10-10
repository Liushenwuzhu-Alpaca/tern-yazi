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
}

type YaziState = {
	seq: number,
	client_id: any,
	parent: ParentInfo?,
	cwd: string,
	files: { string },
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
local function file_icon(fname: string, is_dir: boolean): (string, string)
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

local function fetch_archive_contents(full_url: string, ext: string, state: any)
	if not state.archive_cache then
		state.archive_cache = {}
	end
	if state.archive_cache[full_url] ~= nil then
		return
	end
	state.archive_cache[full_url] = false
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
			if res and res.status == 0 and res.stdout then
				local lines = {}
				for line in res.stdout:gmatch("[^\r\n]+") do
					if #line > 0 then
						table.insert(lines, line)
						if #lines >= 50 then break end
					end
				end
				state.archive_cache[full_url] = lines
			else
				state.archive_cache[full_url] = {}
			end
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
		return nil, ui.text({ ui.span("Preview settings unavailable: " .. tostring(config), "warn") })
	end
	local value = type(config) == "table" and config[kind] or nil
	local limit = spec.default
	if type(value) == "number" and value > 0 and value < math.huge and value % 1 == 0 then
		limit = math.min(value, spec.max)
	end
	local ok_read, content = pcall(tern.fs.read, path, limit)
	if not ok_read then
		return nil, ui.col({
			ui.text({ ui.span("Preview unavailable (" .. fmt_size(limit) .. " limit)", "warn") }),
			ui.text({ ui.span(tostring(content), "muted") }),
		})
	end
	return content, nil
end


-- Rich native preview: Markdown, SVG, Mermaid, Images, Syntax-highlighted code, directory tree
local function build_rich_preview(full_url: string, is_dir: boolean, active_name: string, state: any, cx: any): Node
	if is_dir then
		local ok, list = pcall(tern.fs.list, full_url)
		if ok and list then
			if #list == 0 then
				return ui.text({ ui.span("(empty directory)", "muted") })
			end
			local preview_items = {}
			for i = 1, math.min(18, #list) do
				table.insert(preview_items, ui.text({ ui.span(" " .. list[i], "muted") }))
			end
			if #list > 18 then
				table.insert(preview_items, ui.text({ ui.span(string.format("… and %d more entries", #list - 18), "muted") }))
			end
			return ui.col(preview_items)
		else
			return ui.text({ ui.span("Directory (unreadable)", "muted") })
		end
	end
	local ext = file_ext(full_url)
	local img_mime = mime_for(ext)

	-- 0. Compressed Archive Preview (.zip, .tar, .tar.gz, .tgz, .7z, etc.)
	if is_archive(ext) then
		fetch_archive_contents(full_url, ext, state)
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
			return ui.text({ ui.span("Image preview unavailable: " .. tostring(blob_id), "warn") })
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
	if state.source_client ~= source or state.source_cwd ~= yz.cwd or state.source_hover ~= hovered then
		state.active_file = nil
		state.optimistic_cwd = nil
		state.source_client = source
		state.source_cwd = yz.cwd
		state.source_hover = hovered
	end
end

-- Parent directory entries (from Yazi state, falling back to a live listing).
local function compute_parent(yz: YaziState?, cur_cwd: string): ({ string }?, string?)
	local p = yz and yz.cwd == cur_cwd and yz.parent or nil
	local parent_files = p and p.files
	local parent_cwd = p and p.cwd
	if (not parent_files or #parent_files == 0) and cur_cwd and cur_cwd ~= "/" then
		local parent_dir = cur_cwd:match("(.+)/[^/]+$") or "/"
		local ok_list, list = pcall(tern.fs.list, parent_dir)
		if ok_list and list then
			parent_files = list
			parent_cwd = parent_dir
		end
	end
	if not parent_files or #parent_files == 0 then
		return nil, nil
	end
	return parent_files, parent_cwd
end

-- Current rows come from Yazi's filtered and sorted entries, never a local filter.
local function compute_file_list(state: any, yz: YaziState, cur_cwd: string): { string }
	if cur_cwd == yz.cwd then return yz.files or {} end
	local ok, names = pcall(tern.fs.list, cur_cwd)
	return ok and names or {}
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

local function open_shell(state: any, cx: any, reply: any, query: string, kind: string): boolean
	if state.closing or running[cx.pane] ~= state then return false end
	if reply.orphan then
		cx:toast("info", "Orphan shell queued in Yazi", "Detached execution; output is not shown and completion is not acknowledged")
		return true
	end
	local shell = reply.shell
	if type(reply.id) ~= "string" or not reply.id:match("^[%w-]+$") or type(shell) ~= "table"
		or type(shell.command) ~= "string" or type(shell.cwd) ~= "string" then
		cx:toast("error", "Invalid shell launch", "Yazi did not prepare a visible terminal command")
		return false
	end
	local path = state_dir() .. "/shell-" .. reply.id .. ".json"
	state.shell_handoffs = state.shell_handoffs or {}
	local handoff = { path = path, query = query, kind = kind,
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
		state.cmd_mode, state.cmd_kind, state.cmd_query = true, handoff.kind, handoff.query
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
		state.cmd_mode, state.cmd_kind, state.cmd_query = true, handoff.kind, handoff.query
		cx:toast("error", "Cannot open shell terminal", ev.error or "Visible terminal launch failed")
	end
	cx:render()
end

local function action(state: any, yz: any, cx: any, name: string, args: any)
	request(state, yz, cx, { op = "action", action = name, args = args })
end

local function clear_filter(state: any, yz: any, cx: any)
	state.filter_mode = false
	state.filter_query = ""
	request(state, yz, cx, { op = "filter", query = "" })
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

-- Argument quoting, not shell execution: no substitutions, pipes or globbing.
local function command_words(query: string): (any?, string?)
	local words, buffer = {}, {}
	local quote, escaped, started = nil, false, false
	for i = 1, #query do
		local c = query:sub(i, i)
		if escaped then
			if quote == '"' and c ~= '$' and c ~= '`' and c ~= '"' and c ~= "\\" and c ~= "\n" then
				table.insert(buffer, "\\")
			end
			if c ~= "\n" then table.insert(buffer, c) end
			escaped = false
			started = true
		elseif c == "\\" and quote ~= "'" then escaped = true; started = true
		elseif quote then
			if c == quote then quote = nil else table.insert(buffer, c) end
		elseif c == "'" or c == '"' then quote = c; started = true
		elseif c:match("%s") then
			if started then table.insert(words, table.concat(buffer)); buffer = {}; started = false end
		else table.insert(buffer, c); started = true end
	end
	if quote or escaped then return nil, "Unclosed quote or trailing escape" end
	if started then table.insert(words, table.concat(buffer)) end
	return words, nil
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

-- ---------------------------------------------------------------- Miller columns builders

-- Left Column: Parent directory as a scrollable list; the current
-- directory row is the list selection (native active capsule).
local function build_parent_column(yz: YaziState?, cur_cwd: string, max_lines: number, selected_paths: { [string]: boolean }): Node
	local parent_files, parent_cwd = compute_parent(yz, cur_cwd)
	if not parent_files then
		return ui.card(
			{ ui.span("Parent", "muted") },
			{ ui.text({ ui.span("(root)", "muted") }) }
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
		local is_dir = is_cur
		if not is_dir and parent_cwd then
			local ok_dir = pcall(tern.fs.list, join_path(parent_cwd, fname))
			is_dir = ok_dir
		end
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
local function render_child_directory_preview(full_url: string, dir_name: string, max_lines: number, selected_paths: { [string]: boolean }): Node
	local ok, list = pcall(tern.fs.list, full_url)
	if not ok or not list then
		return ui.card(
			{ ui.span("[" .. dir_name .. "]", "accent") },
			{ ui.text({ ui.span("(unreadable directory)", "muted") }) }
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
		local is_sub_dir = false
		local sub_path = join_path(full_url, cname)
		if tern.fs.exists(sub_path) then
			local ok_sub = pcall(tern.fs.list, sub_path)
			is_sub_dir = ok_sub
		end
		local sym, tone = file_icon(cname, is_sub_dir)
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
	local cwd = state.optimistic_cwd or (yz and yz.cwd) or "/"
	local files = yz and compute_file_list(state, yz, cwd) or {}
	local name = state.active_file or (yz and yz.cwd == cwd and yz.hovered and basename(yz.hovered.url)) or files[1]
	local target = name and join_path(cwd, name)
	local escape = k == "escape" or k == "esc"
	local enter = k == "enter" or k == "return"
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
	if state.cmd_mode then
		if escape then state.cmd_mode = false; state.cmd_query = ""
		elseif enter then
			local query = state.cmd_query or ""
			if state.cmd_kind == "shell" then
				if query == "" then state.cmd_mode = false; return true end
				request(state, yz, cx, { op = "command", action = "shell", args = { query } }, function(reply)
					if reply.ok and open_shell(state, cx, reply, query, "shell") then
						if state.cmd_query == query then state.cmd_mode = false; state.cmd_query = "" end
					end
				end)
				return true
			end
			local words, err = command_words(query)
			if err then cx:toast("error", "Invalid command", err)
			elseif words and #words > 0 then
				local args, positional_only = {}, false
				for i = 2, #words do
					if words[i] == "--" and not positional_only then positional_only = true
					else
						local option = not positional_only and words[i]:match("^%-%-(.+)$")
						if option then
							local field, value = option:match("^([^=]+)=(.*)$")
							args[(field or option):gsub("-", "_")] = value or true
						else table.insert(args, words[i]) end
					end
				end
				request(state, yz, cx, { op = "command", action = words[1], args = args, target = target, cwd = cwd }, function(reply)
					if reply.ok then
						if words[1] == "shell" and not open_shell(state, cx, reply, query, "command") then return end
						if state.cmd_query == query then state.cmd_mode = false; state.cmd_query = "" end
						if reply.confirm_trash and reply.paths and #reply.paths > 0 then state.trash = { cid = safe_cid(yz.client_id), paths = reply.paths } end
						if words[1] ~= "shell" then cx:toast("info", "Yazi accepted command", "Actor accepted; asynchronous work may still be pending") end
					end
				end)
			else state.cmd_mode = false; state.cmd_query = "" end
		elseif k == "backspace" then state.cmd_query = erase_codepoint(state.cmd_query or "")
		else
			local text = typed_text(key)
			if text then state.cmd_query = (state.cmd_query or "") .. text end
		end
		return true
	end
	if state.filter_mode then
		if escape then clear_filter(state, yz, cx)
		elseif enter then state.filter_mode = false
		else
			local query = state.filter_query or ""
			if k == "backspace" then query = erase_codepoint(query)
			else local text = typed_text(key); if not text then return true end; query ..= text end
			state.filter_query = query
			state.active_file = nil
			request(state, yz, cx, { op = "filter", query = query })
		end
		return true
	end
	if key.alt or key.meta then return false end
	if key.ctrl then
		if k == "r" then return true end
		if k ~= "d" and k ~= "u" and k ~= "f" and k ~= "b" then return false end
	end
	if escape and state.filter_query and state.filter_query ~= "" then clear_filter(state, yz, cx); return true end
	if escape and yz and yz.mode and yz.mode ~= "normal" then action(state, yz, cx, "escape", { visual = true }); return true end
	local upper = key.shift or raw == "G"
	local page_down = k == "page_down" or k == "pagedown" or (key.ctrl and (k == "d" or k == "f"))
	local page_up = k == "page_up" or k == "pageup" or (key.ctrl and (k == "u" or k == "b"))
	if page_down or page_up then
		local half = (key.ctrl and (k == "d" or k == "u")) or (not key.ctrl and key.shift)
		action(state, yz, cx, "arrow", { (page_up and "-" or "") .. (half and "50%" or "100%") })
		return true
	end
	if key.ctrl then return false end
	if k == "end" or (k == "g" and upper) then action(state, yz, cx, "arrow", { "bot" }); return true end
	if k == "home" or k == "g" then action(state, yz, cx, "arrow", { "top" }); return true end
	if (key.shift or raw ~= k) and k:match("^[a-z]$") then return false end
	if k == "j" or k == "down" or k == "arrowdown" or k == "arrow_down" or k == "k" or k == "up" or k == "arrowup" or k == "arrow_up" then
		local step = (k == "j" or k == "down" or k == "arrowdown" or k == "arrow_down") and 1 or -1
		action(state, yz, cx, "arrow", { tostring(step) })
	elseif k == "h" or k == "left" or k == "arrowleft" or k == "arrow_left" or k == "backspace" then
		local parent = cwd:match("(.+)/[^/]+$") or "/"
		state.optimistic_cwd = parent; state.active_file = basename(cwd)
		action(state, yz, cx, "cd", { parent })
	elseif k == "l" or k == "right" or k == "arrowright" or k == "arrow_right" or enter then
		if target and pcall(tern.fs.list, target) then
			state.optimistic_cwd = target; state.active_file = nil
			action(state, yz, cx, "cd", { target })
		elseif enter and target then request(state, yz, cx, { op = "open", target = target, cwd = cwd }) end
	elseif k == "space" or k == " " then
		if target then request(state, yz, cx, { op = "toggle", target = target }) end
	elseif k == "v" then action(state, yz, cx, "visual_mode", {})
	elseif k == "y" then
		if target then request(state, yz, cx, { op = "yank", target = target }, function(reply)
			if reply.ok then cx:copy(table.concat(reply.paths or {}, "\n")); cx:toast("info", "Yazi yank accepted", "Copied the same target paths") end
		end) end
	elseif k == "o" then if target then cx:open(target) end
	elseif k == "d" then
		request(state, yz, cx, { op = "trash_prepare", target = target }, function(reply)
			if reply.ok and reply.paths and #reply.paths > 0 then
				state.trash = { cid = safe_cid(yz.client_id), paths = reply.paths }
			elseif reply.ok then cx:toast("info", "Nothing to trash", nil) end
		end)
	elseif k == "/" then state.filter_mode = true
	elseif k == ";" and not key.shift then state.cmd_mode = true; state.cmd_kind = "shell"; state.cmd_query = ""
	elseif k == ":" or k == "colon" or (k == ";" and key.shift) then state.cmd_mode = true; state.cmd_kind = "command"; state.cmd_query = ""
	elseif k == "." or k == "period" then action(state, yz, cx, "hidden", { state = "toggle" })
	elseif k == "x" and target and is_archive(file_ext(target)) then
		local argv = file_ext(target) == "zip" and { "unzip", "-o", target, "-d", cwd } or { "tar", "-xf", target, "-C", cwd }
		local ok, err = pcall(function() tern.process.run(argv, function(res)
			cx:toast(res.status == 0 and "success" or "error", res.status == 0 and "Extracted archive" or "Extraction failed", res.stderr)
		end) end)
		if not ok then cx:toast("error", "Extraction failed", tostring(err)) end
	elseif k == "q" or escape then close_companion(state, cx)
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
			cmd_mode = false,
			cmd_query = "",
			cmd_kind = "command",
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

		local cur_cwd = state.optimistic_cwd or yz.cwd or "/"
		if state.optimistic_cwd and yz.cwd == state.optimistic_cwd then
			state.optimistic_cwd = nil
		end

		-- 1. Top Header: Minimal clean path (collapsing $HOME to ~)
		local home_dir = tern.getenv("HOME") or ""
		local display_path = cur_cwd
		if home_dir ~= "" and display_path:sub(1, #home_dir) == home_dir then
			display_path = "~" .. display_path:sub(#home_dir + 1)
		end

		local top_rows = {}
		local path_header = ui.card({ ui.span(display_path, "accent") }, {})
		path_header.p.role = "yazi.path-header"
		path_header.p.key = "path"
		path_header.p.title = cur_cwd
		table.insert(top_rows, path_header)
		table.insert(top_rows, ui.node("row", { role = "yazi.path-status", gap = "xs", wrap = true }, {
			ui.badge(string.format("%d selected", yz.selected or 0), (yz.selected or 0) > 0 and "accent" or "muted"),
			ui.badge("Synced", "success"),
		}))

		-- 1.1 Filter Bar (when filter active)
		if state.filter_mode or (state.filter_query and state.filter_query ~= "") then
			local f_bar = ui.row({
				ui.badge("Filter [/]", "accent"),
				ui.text({
					ui.span(" " .. (state.filter_query or ""), "strong"),
					ui.span(state.filter_mode and " ▎" or "", "accent"),
				}),
				clickable(ui.badge("Clear [Esc]", "muted"), "clear_filter"),
			})
			table.insert(top_rows, f_bar)
		end

		-- 1.2 Command Bar (when command active)
		if state.cmd_mode or (state.cmd_query and state.cmd_query ~= "") then
			local c_bar = ui.row({
				ui.badge(state.cmd_kind == "shell" and "Shell [;]" or "Command [:]", "warn"),
				ui.text({
					ui.span(" " .. (state.cmd_kind == "shell" and ";" or ":") .. (state.cmd_query or ""), "strong"),
					ui.span(state.cmd_mode and " ▎" or "", "warn"),
				}),
				clickable(ui.badge("Cancel [Esc]", "muted"), "clear_cmd"),
			})
			table.insert(top_rows, c_bar)
		end

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
		local is_active_dir = false
		if active_name then
			local is_exact_hover = (hovered_name == active_name) and yz.hovered
			if is_exact_hover and yz.hovered.dir ~= nil then
				is_active_dir = yz.hovered.dir
			elseif tern.fs.exists(active_full_url) then
				local ok_dir = pcall(tern.fs.list, active_full_url)
				is_active_dir = ok_dir
			end
		end

		-- ---------------- Left Column (Parent Directory)
		local left_col_card = build_parent_column(yz, cur_cwd, col_lines, selected_paths)

		-- ---------------- Middle Column (Current Directory, scrollable)
		local middle_items = {}
		for _, fname in ipairs(file_list) do
			local is_active = (fname == active_name)
			local is_dir = false
			local item_path = join_path(cur_cwd, fname)
			if tern.fs.exists(item_path) then
				local ok_dir = pcall(tern.fs.list, item_path)
				is_dir = ok_dir
			end
			local sym, tone = file_icon(fname, is_dir)
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
			{ viewport_list(middle_items, col_lines, "files_list", cur_idx) }
		)

		-- ---------------- Right Column (Preview / Child Subfolder Expansion)
		local right_col_card
		if not active_name then
			right_col_card = ui.card(
				{ ui.span("Preview", "muted") },
				{ ui.text({ ui.span("(no file selected)", "muted") }) }
			)
		elseif is_active_dir then
			-- Child Directory Content Expansion (exactly like Yazi right pane!)
			right_col_card = render_child_directory_preview(active_full_url, active_name, col_lines, selected_paths)
		else
			-- Rich File Preview (Code, Markdown, Image, Archive table of contents)
			local preview_content = build_rich_preview(active_full_url, false, active_name, state, cx)
			local act_buttons = {
				clickable(ui.el("button", {}, { ui.text("Open in Tern (o)") }), "open_file=" .. active_full_url),
			}
			if is_archive(file_ext(active_name)) then
				table.insert(act_buttons, clickable(ui.el("button", {}, { ui.text("Extract Here (x)") }), "extract_archive=" .. active_full_url))
			end
			table.insert(act_buttons, clickable(ui.el("button", {}, { ui.text("Copy Path (y)") }), "copy_path=" .. active_full_url))
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
		local mode_str = state.cmd_mode and "CMD" or (state.filter_mode and "FILTER" or (yz.mode == "select" and "VIS" or (yz.mode == "unset" and "UNSET" or "NOR")))
		local mode_tone = (state.cmd_mode or (yz.mode and yz.mode ~= "normal")) and "warn" or "accent"

		local perm_str = is_active_dir and "drwxrwxrwx" or "-rw-r--r--"
		local pct_num = (#file_list > 0) and math.floor((cur_idx / #file_list) * 100) or 0
		local pct_str = string.format("%d%%", pct_num)
		local pos_str = string.format("%d/%d", cur_idx, math.max(1, #file_list))

		local status_bar = ui.node("row", { role = "yazi.status", gap = "xs" }, {
			ui.badge(mode_str, mode_tone),
			ui.badge(string.format("%d sel", yz.selected or 0), (yz.selected or 0) > 0 and "warn" or "muted"),
			ui.node("text", { role = "yazi.status-name", spans = { ui.span(active_name or "", "strong") } }),
			ui.text({ ui.span(perm_str, is_active_dir and "accent" or "muted") }),
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
			layer = trash_layer,
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

		if ev.ev == "select" or ev.ev == "activate" then
			local name = list_event_name(ev)
			local cur_cwd_ev = state.optimistic_cwd or cwd
			local base, names
			if ev.id == LIST_IDS.parent_list then
				names, base = compute_parent(yz, cur_cwd_ev)
			elseif ev.id == LIST_IDS.files_list and yz then
				base = cur_cwd_ev
				names = compute_file_list(state, yz, cur_cwd_ev)
			elseif ev.id == LIST_IDS.child_list then
				local active = state.active_file
					or (yz and yz.hovered and yz.hovered.url and basename(yz.hovered.url))
				if active then
					base = join_path(cur_cwd_ev, active)
					local ok, entries = pcall(tern.fs.list, base)
					names = ok and entries or nil
				end
			end
			if not name or not base or not names or not table.find(names, name) then
				return
			end

			local target = join_path(base, name)
			local is_dir = pcall(tern.fs.list, target)
			if ev.ev == "activate" and is_dir then
				state.optimistic_cwd = target
				state.active_file = nil
				action(state, yz, cx, "cd", { target })
			else
				if base ~= cwd then
					state.optimistic_cwd = base
				end
				state.active_file = name
				if ev.ev == "activate" then
					request(state, yz, cx, { op = "open", target = target, cwd = base })
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
			action(state, yz, cx, "cd", { val })
		elseif act == "open_file" and val then
			cx:open(val)
			cx:toast("info", "Opening through Tern", val)
		elseif act == "terminal_here" and val then
			cx:toast("info", "Opening terminal in " .. val, nil)
			pcall(function()
				tern.process.run({ "tern", "split", "down", "--cwd", val }, function() end)
			end)
		elseif act == "leave" then
			local cur_cwd = state.optimistic_cwd or (yz and yz.cwd) or "/"
			local parent_dir = cur_cwd:match("(.+)/[^/]+$") or "/"
			state.optimistic_cwd = parent_dir
			state.active_file = basename(cur_cwd)
			action(state, yz, cx, "cd", { parent_dir })
		elseif act == "clear_filter" then
			clear_filter(state, yz, cx)
		elseif act == "clear_cmd" then
			state.cmd_mode = false
			state.cmd_query = ""
		elseif act == "extract_archive" and val then
			local cur_cwd = state.optimistic_cwd or (yz and yz.cwd) or "."
			local ext = file_ext(val)
			local cmd
			if ext == "zip" then
				cmd = { "unzip", "-o", val, "-d", cur_cwd }
			else
				cmd = { "tar", "-xf", val, "-C", cur_cwd }
			end
			pcall(function()
				tern.process.run(cmd, function(res)
					if res and res.status == 0 then
						cx:toast("success", "Extracted archive successfully", basename(val))
					else
						cx:toast("error", "Failed to extract archive", basename(val))
					end
				end)
			end)
			-- Handled by view re-run
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
