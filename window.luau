-- window.luau: Window half of tern-yazi plugin.
-- Controls the "companion" block pane, manages PiP float/dock toggle,
-- and periodically pokes the block when state files change.

local PLUGIN_ID = "tern-yazi"
local BLOCK_KIND = PLUGIN_ID .. ".companion"
local DEFAULT_CORNER = "tr"

local companion = { pane = nil, seq = -1 }

-- ---------------------------------------------------------------- helpers

local function companion_alive(cx): boolean
	if not companion.pane then
		return false
	end
	local ok, info = pcall(function()
		return cx.session:pane(companion.pane)
	end)
	return ok and info ~= nil
end

local function adopt_companion(cx): boolean
	for _, info in ipairs(cx.session:panes()) do
		if info.block == BLOCK_KIND then
			companion.pane = info.pane
			return true
		end
	end
	return false
end

local function open_companion(cx)
	if adopt_companion(cx) then
		return
	end
	local pane = cx:new_block(BLOCK_KIND, nil, "beside", { focus = false })
	if not pane then
		tern.log.warn("new_block failed", BLOCK_KIND)
		return
	end
	companion.pane = pane
	local ok, err = cx.layout:float(pane, nil, DEFAULT_CORNER)
	if not ok then
		tern.log.warn("float failed", err)
	end
end

local function toggle(cx)
	if companion_alive(cx) then
		local info = cx.session:pane(companion.pane)
		if info and info.floating then
			cx.layout:dock(companion.pane)
		else
			local ok, err = cx.layout:float(companion.pane, nil, DEFAULT_CORNER)
			if not ok then
				tern.log.warn("float failed", err)
			end
		end
	else
		open_companion(cx)
	end
end

local function state_dir(): string
	local dir = tern.getenv("XDG_RUNTIME_DIR")
	if dir and dir ~= "" then
		return dir .. "/tern-yazi"
	end
	return "/tmp/tern-yazi"
end
-- Watch the plugin store independently of Yazi snapshots so saved limits apply live.
local preview_config_stamp = nil
local function poke_preview_config(cx)
	local ok, stamp = pcall(function()
		return tern.json.encode(tern.kv.get("preview_limits") or tern.json.null)
	end)
	stamp = (ok and "ok:" or "error:") .. tostring(stamp)
	if stamp == preview_config_stamp then
		return
	end
	preview_config_stamp = stamp
	if companion_alive(cx) or adopt_companion(cx) then
		cx.session:event(companion.pane, { ev = "action", id = "main.poll", act = "poll" })
	end
end


local function poke(cx)
	poke_preview_config(cx)
	local sdir = state_dir()
	if not tern.fs.exists(sdir) then
		return
	end
	local ok_list, entries = pcall(tern.fs.list, sdir)
	if not ok_list or not entries then
		return
	end

	local newest_score = -1
	for _, fname in ipairs(entries) do
		if fname:match("^state%-.+%.json$") then
			local ok_read, content = pcall(tern.fs.read, sdir .. "/" .. fname, 256 * 1024)
			if ok_read and content then
				local ts = content:match('"ts"%s*:%s*([%d%.]+)')
				local num = tonumber(ts) or tonumber(content:match('"seq"%s*:%s*(%d+)'))
				if num and num > newest_score then
					newest_score = num
				end
			end
		end
	end

	if newest_score > companion.seq then
		companion.seq = newest_score
		if companion_alive(cx) then
			cx.session:event(companion.pane, { ev = "action", id = "main.poll", act = "poll" })
		else
			open_companion(cx)
		end
	end
end

-- ---------------------------------------------------------------- events

tern.on("window_start", function(cx)
	poke(cx)
end)

tern.on("pane_closed", function(ev, cx)
	if companion.pane and ev.pane == companion.pane then
		companion.pane = nil
	end
end)

local function arm_timer()
	tern.timer(100, function(cx)
		if cx then
			pcall(poke, cx)
		end
		arm_timer()
	end)
end
arm_timer()

-- ---------------------------------------------------------------- commands

tern.command({
	id = "toggle",
	title = "Yazi: Toggle Companion Panel",
	group = "Yazi",
	run = function(cx)
		toggle(cx)
	end,
})
tern.bind("ctrl+alt+y", function(cx)
	toggle(cx)
end)

tern.command({
	id = "preview_limits",
	title = "Yazi: Configure Preview Limits",
	group = "Yazi",
	run = function(cx)
		local ok, err = pcall(function()
			local defaults = {
				image_bytes = 16 * 1024 * 1024,
				markdown_bytes = 1024 * 1024,
				code_bytes = 1024 * 1024,
				mermaid_bytes = 256 * 1024,
			}
			local config = tern.kv.get("preview_limits")
			if config == nil then
				tern.kv.set("preview_limits", defaults)
			elseif type(config) == "table" then
				local changed = false
				for name, value in pairs(defaults) do
					if config[name] == nil then
						config[name] = value
						changed = true
					end
				end
				if changed then tern.kv.set("preview_limits", config) end
			end
		end)
		if not ok then
			cx:toast("error", "Cannot initialize preview limits", tostring(err))
		end
		cx:open(tern.plugin.data .. "/kv.json")
		cx:toast("info", "Edit preview_limits and save to apply", "Positive integer bytes; caps: image 16 MiB, Markdown/code 4 MiB, Mermaid 1 MiB")
	end,
})
