-- tern.yazi: exports yazi's state to the tern-yazi companion over DDS, and
-- receives companion commands. Zero patches — plugin API + DDS only.
--
-- Install: link this directory into ~/.config/yazi/plugins/, then add
--   require("tern"):setup()
-- to ~/.config/yazi/init.lua.

local M = {}

-- Custom kinds (alphanumeric + dashes; must not collide with builtin kinds).
local KIND_HOVER = "tern-hover"
local KIND_CD = "tern-cd"
local KIND_CMD = "tern-cmd"
local KIND_ACK = "tern-ack"

-- Broadcast (receiver 0) to every remote subscriber, e.g. `ya sub <kind>`.
local function broadcast(kind, value)
	ps.pub_to(0, kind, value)
end

-- ps.sub callbacks observe the manager mid-update; defer the cx read onto the
-- async runtime and re-enter the sync context once the input batch has settled.
local function after_settle(read)
	ya.async(function()
		broadcast(read.kind, ya.sync(read.fn)())
	end)
end

local function read_hovered()
	local h = cx.active.current.hovered
	return { url = h and tostring(h.url) or nil }
end

local function read_folder()
	local cur = cx.active.current
	local names = {}
	for i = 1, #cur.files do
		local url = cur.files[i].url
		names[i] = url.name or tostring(url)
	end
	return { url = tostring(cur.cwd), files = names }
end

function M:setup()
	-- Builtin local event bodies carry only { tab }; snapshot `cx` instead.
	ps.sub("hover", function()
		after_settle({ kind = KIND_HOVER, fn = read_hovered })
	end)
	ps.sub("cd", function()
		after_settle({ kind = KIND_CD, fn = read_folder })
	end)

	-- Companion command channel.
	--   {"op":"hello"} -> full snapshot (companion startup / reattach)
	--   anything else  -> ack echo (round-trip probe)
	ps.sub_remote(KIND_CMD, function(body)
		if type(body) == "table" and body.op == "hello" then
			after_settle({ kind = KIND_CD, fn = read_folder })
			after_settle({ kind = KIND_HOVER, fn = read_hovered })
		else
			broadcast(KIND_ACK, { got = body })
		end
	end)
end

return M
