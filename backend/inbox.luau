-- Bounded File Inbox reads used by the Host and Window backend modules.
local M = {}

function M.state_dir(): string
	local dir = tern.getenv("XDG_RUNTIME_DIR")
	if dir and dir ~= "" then
		return dir .. "/tern-yazi"
	end
	return "/tmp/tern-yazi"
end

function M.read_json(path: string): any?
	local ok, text = pcall(tern.fs.read, path, 256 * 1024)
	if not ok then return nil end
	local decoded, value = pcall(tern.json.decode, text)
	return decoded and type(value) == "table" and value or nil
end

function M.lease_hint(cid: string): (string?, number?, string?)
	local ok, text = pcall(tern.fs.read, M.state_dir() .. "/managed-" .. cid .. ".lease", 4096)
	if not ok then return nil, nil, nil end
	local token, pane, _, _, nonce = text:match("^(%S+)%s+(%d+)%s+(%d+)%s+(%d+)%s+(%S+)")
	if not token then token, pane = text:match("^(%S+)%s+(%d+)") end
	return token, tonumber(pane), nonce
end

return M
