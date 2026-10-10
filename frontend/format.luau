-- Presentation-only metadata labels and adapters for authoritative Yazi glyphs.
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

local function fmt_permissions(permissions: string?): string
	-- Uppercase S/T encode missing execute bits; preserve Yazi's exact text.
	return permissions or "-"
end

-- Reuse the preview/model classification; never infer metadata from file content.
local function preview_kind(fname: string, data: any): string
	if data.tag == "image" then return data.mime end
	if data.tag == "archive" then return "archive" end
	if data.tag == "binary" then return "binary" end
	if data.tag == "markdown" or data.tag == "mermaid" then return data.tag end
	return data.lang or model.detect_lang(file_ext(fname))
end
-- Preserve the existing semantic color policy without selecting any glyphs.
local function file_tone(fname: string, is_dir: boolean?): string
	if is_dir == nil then return "muted" end
	if is_dir then return fname:sub(1, 1) == "." and "muted" or "accent" end
	local ext = file_ext(fname)
	if is_archive(ext) then
		return "warn"
	elseif ext == "lua" or ext == "luau" then
		return "accent"
	elseif ext == "py" then
		return "warn"
	elseif ext == "rs" then
		return "error"
	elseif ext == "js" or ext == "ts" or ext == "jsx" or ext == "tsx" then
		return "warn"
	elseif ext == "sh" or ext == "bash" or ext == "zsh" then
		return "success"
	elseif ext == "c" or ext == "cpp" or ext == "h" or ext == "hpp" or ext == "go" then
		return "accent"
	elseif ext == "md" or ext == "markdown" then
		return "accent"
	elseif ext == "png" or ext == "jpg" or ext == "jpeg" or ext == "svg" or ext == "gif" or ext == "webp" then
		return "accent"
	elseif ext == "mp4" or ext == "mkv" or ext == "webm" or ext == "mp3" or ext == "wav" then
		return "accent"
	elseif ext == "pdf" then
		return "error"
	end
	return "muted"
end

-- Yazi owns real-file glyph selection. Missing metadata is not a virtual file.
-- Tern spans accept semantic tones, not Yazi's Color userdata or raw RGB values.
local function file_icon(icon: (string | boolean)?, fname: string, is_dir: boolean?): (string, string)
	local tone = file_tone(fname, is_dir)
	if type(icon) ~= "string" or icon == "" then return "", tone end
	return icon .. " ", tone
end

-- Archive member strings have no live Yazi File, so use generic glyphs only.
local function virtual_icon(fname: string, is_dir: boolean): (string, string)
	return is_dir and " " or " ", file_tone(fname, is_dir)
end

M.fmt_size = fmt_size
M.fmt_permissions = fmt_permissions
M.preview_kind = preview_kind
M.file_icon = file_icon
M.virtual_icon = virtual_icon
return M
