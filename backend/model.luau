-- Pure snapshot and file metadata helpers shared by the host frontend.
local path = require('../shared/path')
local basename, join_path = path.basename, path.join_path
local M = {}

export type HoverInfo = {
	url: string?,
	dir: boolean?,
	size: number?,
	mtime: number?,
	permissions: string?,
	selected: number?,
}

export type TaskInfo = {
	total: number,
	succ: number,
	fail: number,
	found: number?,
	processed: number?,
}

-- Boolean values on the wire are false: Yazi explicitly returned no icon.
export type FileIcons = { [string]: string | boolean }

export type ParentInfo = {
	cwd: string?,
	files: { string }?,
	file_dirs: { [string]: boolean }?,
	file_icons: FileIcons?,
	icon_overrides: FileIcons?,
	file_count: number?,
}

export type YaziState = {
	seq: number,
	client_id: any,
	listing_epoch: string,
	listing_revision: number,
	cursor: number?,
	parent: ParentInfo?,
	preview: ParentInfo?,
	cwd: string,
	files: { string },
	file_dirs: { [string]: boolean }?,
	file_icons: FileIcons?,
	icon_overrides: FileIcons?,
	file_count: number?,
	filter: (string | boolean)?,
	finder: (string | boolean)?,
	selected: number,
	selected_urls: { string }?,
	mode: string?,
	marked_urls: { string }?,
	hovered: HoverInfo?,
	tasks: TaskInfo?,
}


-- Exact comparisons, not cwd/count shortcuts, retain immutable listing references.
local function same_value(a: any, b: any): boolean
	if a == b then return true end
	if type(a) ~= "table" or type(b) ~= "table" then return false end
	for key, value in pairs(a) do
		if not same_value(value, b[key]) then return false end
	end
	for key in pairs(b) do if a[key] == nil then return false end end
	return true
end

local function reuse_folder(folder: any, previous: any): any
	if not folder or not previous or folder.cwd ~= previous.cwd then return folder end
	for _, field in ipairs({ "files", "file_dirs", "file_icons" }) do
		if same_value(folder[field], previous[field]) then folder[field] = previous[field] end
	end
	return same_value(folder, previous) and previous or folder
end

local function reuse_listing(listing: any, previous: any): any
	if not previous or listing.epoch ~= previous.epoch then return listing end
	reuse_folder(listing, previous)
	listing.parent = reuse_folder(listing.parent, previous.parent)
	listing.preview = reuse_folder(listing.preview, previous.preview)
	for _, field in ipairs({ "selected_urls", "marked_urls" }) do
		if same_value(listing[field], previous[field]) then listing[field] = previous[field] end
	end
	return listing
end

local function same_snapshot(a: any, b: any): boolean
	if not a or not b then return a == b end
	for key, value in pairs(a) do
		if key ~= "seq" and key ~= "ts" and key ~= "listing_revision" and not same_value(value, b[key]) then return false end
	end
	for key in pairs(b) do
		if key ~= "seq" and key ~= "ts" and key ~= "listing_revision" and a[key] == nil then return false end
	end
	return true
end

local function reuse_dynamic(yz: any, previous: any)
	if not previous or yz.listing_epoch ~= previous.listing_epoch then return end
	for _, field in ipairs({ "selected_urls", "marked_urls", "icon_overrides" }) do
		if same_value(yz[field], previous[field]) then yz[field] = previous[field] end
	end
	for _, field in ipairs({ "parent", "preview" }) do
		local folder, old = yz[field], previous[field]
		if folder and old and folder.cwd == old.cwd then
			if same_value(folder.icon_overrides, old.icon_overrides) then folder.icon_overrides = old.icon_overrides end
			if same_value(folder, old) then yz[field] = old end
		end
	end
end
local function file_ext(path: string): string
	local ext = path:match("%.([^./\\]+)$")
	return ext and ext:lower() or ""
end

local LANGUAGES = {
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

local function detect_lang(ext: string): string
	return LANGUAGES[ext] or "text"
end

local MIME_TYPES = {
	png = "image/png",
	jpg = "image/jpeg",
	jpeg = "image/jpeg",
	gif = "image/gif",
	webp = "image/webp",
	svg = "image/svg+xml",
}

local function mime_for(ext: string): string?
	return MIME_TYPES[ext]
end

local function is_archive(ext: string): boolean
	return ext == "zip" or ext == "tar" or ext == "gz" or ext == "tgz" or ext == "bz2" or ext == "xz" or ext == "7z" or ext == "rar" or ext == "zst"
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

local function compute_parent(yz: YaziState?, cur_cwd: string): ({ string }?, string?, { [string]: boolean }?, FileIcons?)
	if cur_cwd == "/" then return nil, nil, nil, nil end
	local parent_cwd = cur_cwd:match("(.+)/[^/]+$") or "/"
	local folder = compute_directory(yz, parent_cwd)
	return folder and folder.files, parent_cwd, folder and folder.file_dirs, folder and folder.file_icons
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

-- References remain owned by the immutable Yazi snapshot.
local function presentation(state: any, yz: YaziState): any
	local cur_cwd = state.optimistic_cwd or yz.cwd or "/"
	local cache = state.presentation_cache
	if cache and cache.snapshot == yz and cache.cwd == cur_cwd and cache.active_file == state.active_file then
		return cache.prepared
	end
	cache = cache or {}
	state.presentation_cache = cache
	local file_list = compute_file_list(state, yz, cur_cwd)
	local folder = compute_directory(yz, cur_cwd)
	if cache.files ~= file_list then
		local indices = {}
		for index, name in ipairs(file_list) do indices[name] = index end
		cache.files, cache.indices = file_list, indices
	end
	if cache.selected_urls ~= yz.selected_urls or not cache.selected_paths then
		cache.selected_urls, cache.selected_paths = yz.selected_urls, selection_paths(yz)
	end
	if cache.marked_urls ~= yz.marked_urls or not cache.marked_paths then
		local marked_paths = {}
		for _, value in ipairs(yz.marked_urls or {}) do marked_paths[value] = true end
		cache.marked_urls, cache.marked_paths = yz.marked_urls, marked_paths
	end
	local hovered_url = yz.cwd == cur_cwd and yz.hovered and yz.hovered.url
	local active_name = state.active_file or (hovered_url and basename(hovered_url)) or file_list[1]
	local cur_idx = active_name and cache.indices[active_name] or 1
	local active_full_url = active_name and join_path(cur_cwd, active_name) or cur_cwd
	local active_hovered = yz.hovered and yz.hovered.url == active_full_url and yz.hovered or nil
	local prepared = {
		cur_cwd = cur_cwd, file_list = file_list, selected_paths = cache.selected_paths,
		file_icons = folder and folder.file_icons, icon_overrides = folder and folder.icon_overrides,
		marked_paths = cache.marked_paths, active_name = active_name, cur_idx = cur_idx or 1,
		active_full_url = active_full_url, active_hovered = active_hovered,
		is_active_dir = active_name and file_is_dir(yz, cur_cwd, active_name),
	}
	cache.snapshot, cache.cwd, cache.active_file, cache.prepared = yz, cur_cwd, state.active_file, prepared
	return prepared
end

M.file_ext = file_ext
M.detect_lang = detect_lang
M.mime_for = mime_for
M.is_archive = is_archive
M.compute_directory = compute_directory
M.file_is_dir = file_is_dir
M.compute_parent = compute_parent
M.compute_file_list = compute_file_list
M.selection_paths = selection_paths
M.presentation = presentation
M.reuse_listing = reuse_listing
M.reuse_dynamic = reuse_dynamic
M.same_snapshot = same_snapshot
return M
