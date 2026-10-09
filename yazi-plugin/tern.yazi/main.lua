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

local function hovered_url()
	local h = cx.active.current.hovered
	return h and tostring(h.url) or nil
end

local function publish_hover()
	broadcast(KIND_HOVER, { url = hovered_url() })
end

local function publish_cd()
	local cur = cx.active.current
	broadcast(KIND_CD, { url = tostring(cur.cwd), files = #cur.files })
end

function M:setup()
	-- Builtin local event bodies carry only { tab }; read `cx` for the real state.
	ps.sub("hover", publish_hover)
	ps.sub("cd", publish_cd)

	-- Companion command channel; ack by broadcast so `ya sub tern-ack` sees it.
	ps.sub_remote(KIND_CMD, function(body)
		broadcast(KIND_ACK, { got = body, url = hovered_url() })
	end)
end

return M
