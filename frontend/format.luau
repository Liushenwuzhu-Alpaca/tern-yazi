-- Presentation-only size labels and file glyphs.
local model = require('../backend/model')
local file_ext, is_archive = model.file_ext, model.is_archive
local M = {}

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

M.fmt_size = fmt_size
M.file_icon = file_icon
return M
