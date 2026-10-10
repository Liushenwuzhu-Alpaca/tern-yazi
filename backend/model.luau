-- Pure snapshot and file metadata helpers shared by the host frontend.
local path = require('../shared/path')
local basename, join_path = path.basename, path.join_path
local M = {}

export type HoverInfo = {
	url: string?,
	dir: boolean?,
	size: number?,
	mtime: number?,
	selected: number?,
}

export type TaskInfo = {
	total: number,
	succ: number,
	fail: number,
	found: number?,
	processed: number?,
}

export type ParentInfo = {
	cwd: string?,
	files: { string }?,
	file_dirs: { [string]: boolean }?,
}

export type YaziState = {
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

-- References remain owned by the immutable Yazi snapshot.
local function presentation(state: any, yz: YaziState): any
	local cur_cwd = state.optimistic_cwd or yz.cwd or "/"
	local file_list = compute_file_list(state, yz, cur_cwd)
	local marked_paths = {}
	for _, value in ipairs(yz.marked_urls or {}) do marked_paths[value] = true end
	local hovered_url = yz.cwd == cur_cwd and yz.hovered and yz.hovered.url
	local active_name = state.active_file or (hovered_url and basename(hovered_url)) or file_list[1]
	local cur_idx = 1
	for idx, fname in ipairs(file_list) do
		if fname == active_name then cur_idx = idx; break end
	end
	return {
		cur_cwd = cur_cwd, file_list = file_list, selected_paths = selection_paths(yz),
		marked_paths = marked_paths, active_name = active_name, cur_idx = cur_idx,
		active_full_url = active_name and join_path(cur_cwd, active_name) or cur_cwd,
		is_active_dir = active_name and file_is_dir(yz, cur_cwd, active_name),
	}
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
return M
