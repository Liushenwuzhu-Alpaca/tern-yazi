-- Pure path and client-ID helpers shared by the native layers.
local M = {}

function M.basename(path: string?): string
	if not path or path == "" then
		return "Yazi"
	end
	local name = path:match("([^/]+)/*$")
	return name or path
end

function M.safe_cid(cid: any): string?
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

function M.join_path(dir: string?, name: string?): string
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

return M
