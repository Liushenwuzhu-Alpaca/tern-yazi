-- Pure classification shared by the early router and validated backend handlers.
local M = {}

function M.parse(path: string, directory: string): (string?, string?, string?, number?)
	local prefix = directory .. "/"
	if path:sub(1, #prefix) ~= prefix then return nil end
	local name = path:sub(#prefix + 1)
	local cid, token, pane = name:match("^return%-(%d+)%-([%w-]+)%.(%d+)$")
	if cid then return "return", cid, token, tonumber(pane) end
	local id = name:match("^managed%-(%d+)%.json$")
	if id then return "managed", id end
	id = name:match("^shell%-([%w-]+)%.json$")
	if id then return "shell", id end
	id = name:match("^files%-([%w-]+)%.json$")
	if id then return "files", id end
	id = name:match("^native%-shell%-([%w-]+)%.json$")
	if id then return "native-shell", id end
	return nil
end

return M
