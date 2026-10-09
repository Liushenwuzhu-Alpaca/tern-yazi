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

local function poke(cx)
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
