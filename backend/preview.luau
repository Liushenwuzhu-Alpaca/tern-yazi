-- Bounded preview/config/archive IO. Payloads contain data, never native nodes.
local model = require('./model')
local session = require('./session')
local file_ext, mime_for, is_archive = model.file_ext, model.mime_for, model.is_archive
local M = {}

local function fetch_archive_contents(full_url: string, ext: string, state: any)
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
			if state.closing or state.archive_cache ~= cache or not session.current(state) then return end
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
			state.on_change()
		end)
	end)
end

-- Plugin-scoped byte limits; bounded reads reject whole oversized files, not prefixes.
local PREVIEW_LIMITS = require('./preview_policy').limits

local function read_preview(path: string, kind: string): (string?, any?)
	local spec = PREVIEW_LIMITS[kind]
	local ok_config, config = pcall(tern.kv.get, "preview_limits")
	if not ok_config then return nil, { tag = "settings-unavailable" } end
	local value = type(config) == "table" and config[kind] or nil
	local limit = spec.default
	if type(value) == "number" and value > 0 and value < math.huge and value % 1 == 0 then
		limit = math.min(value, spec.max)
	end
	local ok_read, content = pcall(tern.fs.read, path, limit)
	if not ok_read then return nil, { tag = "unavailable", limit = limit } end
	return content, nil
end

local function prepare(state: any, full_url: string): any
	local ext = file_ext(full_url)
	if is_archive(ext) then
		fetch_archive_contents(full_url, ext, state)
		local cached = state.archive_cache[full_url]
		return { tag = "archive", status = type(cached) == "table" and #cached > 0 and "ready"
			or (cached == false and "loading" or "empty"), items = cached }
	end
	local mime = mime_for(ext)
	local kind = "code_bytes"
	if mime then kind = "image_bytes"
	elseif ext == "mermaid" or ext == "mmd" then kind = "mermaid_bytes"
	elseif ext == "md" or ext == "markdown" then kind = "markdown_bytes" end
	local content, failure = read_preview(full_url, kind)
	if not content then return failure end
	if mime then return { tag = "image", content = content, mime = mime, ext = ext } end
	if ext == "mermaid" or ext == "mmd" then return { tag = "mermaid", content = content } end
	if ext == "md" or ext == "markdown" then return { tag = "markdown", content = content } end
	if content:find("\0") then return { tag = "binary", content = content } end
	return { tag = "code", content = content, lang = model.detect_lang(ext) }
end

local function init(state: any)
	state.archive_cache = {}
end

local function sync(state: any, yz: any)
	-- Async results never outlive the directory/snapshot that requested them.
	local stamp = tostring(yz.client_id) .. ":" .. tostring(yz.seq) .. ":" .. yz.cwd
	if state.preview_stamp ~= stamp then
		state.preview_stamp = stamp
		state.archive_cache = {}
	end
end

local function config_stamp(): string
	local ok, config = pcall(function() return tern.json.encode(tern.kv.get("preview_limits")) end)
	return tostring(ok) .. tostring(config)
end

M.init = init
M.sync = sync
M.prepare = prepare
M.config_stamp = config_stamp
return M
