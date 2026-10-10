-- Native window presentation and registration. Protocol decisions stay backend-owned.
local backend = require('../backend/window')
local BLOCK_KIND = "tern-yazi.companion"
local DEFAULT_CORNER = "tr"
local companion = { pane = nil }
local overlay_key = "managed_overlays"
local overlay_restore = {}
local Window = {}

local function send_event(cx, report)
	if report and cx.session:pane(report.pane) then cx.session:event(report.pane, report.event) end
end
local function poll_pane(cx, pane)
	cx.session:event(pane, { ev = "action", id = "main.poll", act = "poll" })
end
local function overlay_store(): any
	local value = tern.kv.get(overlay_key)
	return type(value) == "table" and value or {}
end
local function pane_zoomed(cx, pane): boolean
	local tab = cx.session:tab_of(pane)
	local layout = tab and cx.session:layout(tab)
	return layout and layout.zoomed or false
end
-- Read-only metadata callbacks never retain a window context in the backend.
local function query(cx)
	return {
		pane = function(pane) return cx.session:pane(pane) end,
		tab_of = function(pane) return cx.session:tab_of(pane) end,
		zoomed = function(pane) return pane_zoomed(cx, pane) end,
	}
end
local function zoom_pane(cx, pane, zoomed)
	local tab = cx.session:tab_of(pane)
	local layout = tab and cx.session:layout(tab)
	if not layout then return end
	cx.layout:focus(pane)
	if layout.zoomed ~= zoomed then cx:command("zoom") end
end
local function remember_overlay(cx, record)
	local store = overlay_store()
	local saved = store[record.key]
	if type(saved) == "table" and saved.owner_pane == record.owner_pane then
		overlay_restore[record.key] = saved.was_zoomed == true
	else
		overlay_restore[record.key] = pane_zoomed(cx, record.owner_pane)
		store[record.key] = { owner_pane = record.owner_pane, was_zoomed = overlay_restore[record.key] }
		tern.kv.set(overlay_key, store)
	end
end
local function restore_overlay(cx, record)
	local owner = cx.session:pane(record.owner_pane)
	if owner then
		local focused = cx.session:focused()
		-- Do not steal focus from another client or a different tab.
		if focused == record.pane or focused == record.owner_pane then
			zoom_pane(cx, record.owner_pane, overlay_restore[record.key] == true)
		end
	end
	local store = overlay_store()
	overlay_restore[record.key] = nil
	store[record.key] = nil
	tern.kv.set(overlay_key, store)
end
local function companion_alive(cx): boolean
	return companion.pane ~= nil and cx.session:pane(companion.pane) ~= nil
end

-- Ordinary Yazi remains attach-only, never inferred live from a saved snapshot.
local function open_companion(cx)
	if companion_alive(cx) then return end
	local pane = cx:new_block(BLOCK_KIND, nil, "beside", { focus = true })
	if not pane then tern.log.warn("new_block failed", BLOCK_KIND); return end
	companion.pane = pane
	local ok, err = cx.layout:float(pane, nil, DEFAULT_CORNER)
	if not ok then tern.log.warn("float failed", err) end
	poll_pane(cx, pane)
end
local function toggle(cx)
	local focused = cx.session:focused()
	local pane = companion_alive(cx) and companion.pane or nil
	local record = backend.focused_managed(focused, query(cx))
	if record then zoom_pane(cx, record.pane, true); return end
	if not pane then open_companion(cx); return end
	local info = cx.session:pane(pane)
	if info and info.floating then cx.layout:dock(pane)
	else
		local ok, err = cx.layout:float(pane, nil, DEFAULT_CORNER)
		if not ok then tern.log.warn("float failed", err) end
	end
end
local function launch_managed(cx, record)
	local plan = backend.managed_launch_plan(record)
	-- new_block is relative to focus; explicitly select the launching shell.
	cx.layout:focus(plan.owner_pane)
	remember_overlay(cx, record)
	local pane = cx:new_block(BLOCK_KIND, plan.args, "beside", { focus = true })
	backend.managed_launched(record, pane)
	if pane then zoom_pane(cx, pane, true) end
end
local function poke(cx)
	local panes = {}
	for _, info in ipairs(cx.session:panes()) do panes[info.pane] = info end
	if backend.preview_config_changed() then
		for _, info in pairs(panes) do
			if info.block == BLOCK_KIND then poll_pane(cx, info.pane) end
		end
	end
	-- Poll saved unmanaged blocks too; hosts require their own live evidence.
	for _, info in pairs(panes) do
		if info.block == BLOCK_KIND then poll_pane(cx, info.pane) end
	end
	local scan = backend.discover_managed(panes)
	if not scan then return end
	local metadata = query(cx)
	while true do
		local candidate = backend.next_managed(scan)
		if not candidate then break end
		local record = candidate.record
		if candidate.adopt then remember_overlay(cx, record)
		elseif candidate.launch then launch_managed(cx, record) end
		if not backend.managed_active(candidate, metadata) then continue end
		if not record.pane then launch_managed(cx, record) end
		if not record.pane then continue end
		local lease = backend.managed_lease(record)
		if lease == "relaunch" then
			-- A recycled hint gets one newly pinned block, never the first companion.
			launch_managed(cx, record)
		elseif lease == "failed" then
			cx:toast("error", "Cannot attach native Yazi", "The block did not acknowledge its client/token")
			continue
		end
		local report = backend.managed_poll(record)
		cx.session:event(report.pane, report.event)
	end
	for _, pane in ipairs(backend.finish_managed_scan(scan)) do poll_pane(cx, pane) end
end
local function expire_native_terminals(cx, closed_pane)
	for _, pane in ipairs(backend.expire_native_terminals(query(cx), closed_pane)) do cx.layout:close(pane) end
end
local function arm_timer()
	tern.timer(500, function(cx)
		if cx then
			for _, report in ipairs(backend.expire_handoffs(query(cx))) do send_event(cx, report) end
			local ok, err = pcall(poke, cx)
			if not ok then tern.log.warn("Yazi managed poll failed", tostring(err)) end
			expire_native_terminals(cx)
		end
		arm_timer()
	end)
end

function Window.register()
	-- Launcher file-open wakes idle windows without depending on the timer.
	tern.route.open(function(req, cx)
		local result = backend.managed_request(req, query(cx))
		if not result then return nil end
		if result.wake then poke(cx)
		else cx:toast("error", "Cannot open managed Yazi", "The launching pane or managed session is missing") end
		return { handled = true }
	end)
	tern.on("window_start", function(cx) poke(cx) end)
	tern.on("pane_closed", function(ev, cx)
		if companion.pane == ev.pane then companion.pane = nil end
		for _, record in ipairs(backend.managed_closed(ev.pane)) do restore_overlay(cx, record) end
	end)

	tern.route.open(function(req, cx)
		local result = backend.shell_request(req, query(cx))
		if not result then return nil end
		if result.unavailable then
			cx:toast("error", "Cannot open shell terminal", "The originating Yazi session is unavailable")
		end
		send_event(cx, result.send)
		local launch = result.launch
		if launch then
			local was_zoomed = pane_zoomed(cx, launch.pane)
			local ok, terminal = pcall(function()
				return cx.layout:split(launch.pane, "right", { cwd = launch.cwd,
					command = launch.command }, { focus = true })
			end)
			local launched = ok and terminal or nil
			local report = backend.shell_completed(launch, launched, was_zoomed,
				not launched and tostring(terminal or "Terminal launch failed") or nil)
			if launched then zoom_pane(cx, launched, true) end
			send_event(cx, report)
		end
		return { handled = true }
	end)

	tern.route.open(function(req, cx)
		local result = backend.files_request(req, query(cx))
		if not result then return nil end
		if result.message then cx:toast("error", "Cannot open selected files", result.message) end
		send_event(cx, result.send)
		local batch = result.batch
		if batch then
			-- Anchor every split in the source tab; native opens bypass routes.
			zoom_pane(cx, batch.pane, false)
			local failures = {}
			for index, path in ipairs(batch.paths) do
				local ok, err = pcall(function() cx:open(path, "beside", { focus = index == #batch.paths }) end)
				if not ok then table.insert(failures, path .. ": " .. tostring(err)) end
			end
			send_event(cx, backend.files_completed(batch, failures))
		end
		return { handled = true }
	end)

	tern.route.open(function(req, cx)
		local result = backend.native_request(req, query(cx))
		if not result then return nil end
		local launch = result.launch
		if launch then
			local ok, terminal = pcall(function()
				return cx.layout:split(launch.pane, "right", { cwd = launch.cwd,
					command = launch.command }, { focus = true })
			end)
			local launched = ok and terminal or nil
			backend.native_completed(launch, launched)
			if not launched then
				cx:toast("error", "Cannot open native Yazi terminal", "Terminal launch failed; the native request will expire")
			else zoom_pane(cx, launched, true) end
		end
		return { handled = true }
	end)

	tern.on("pane_closed", function(ev, cx) expire_native_terminals(cx, ev.pane) end)
	tern.on("pane_closed", function(ev, cx)
		local previous = backend.shell_closed(ev.pane)
		if not previous then return end
		local target = cx.session:pane(previous.pane) and previous.pane or previous.owner_pane
		local focused = cx.session:focused()
		if target and cx.session:pane(target) and (focused == ev.pane or focused == target) then
			zoom_pane(cx, target, previous.was_zoomed == true)
			if target == previous.pane then poll_pane(cx, target) end
		end
	end)
	arm_timer()

	tern.command({ id = "toggle", title = "Yazi: Toggle Companion Panel", group = "Yazi", run = toggle })
	tern.bind("ctrl+alt+y", toggle)
	tern.command({
		id = "preview_limits",
		title = "Yazi: Configure Preview Limits",
		group = "Yazi",
		run = function(cx)
			local ok, err = pcall(backend.initialize_preview_limits)
			if not ok then cx:toast("error", "Cannot initialize preview limits", tostring(err)) end
			cx:open(tern.plugin.data .. "/kv.json")
			cx:toast("info", "Edit preview_limits and save to apply", "Positive integer bytes; caps: image 16 MiB, Markdown/code 4 MiB, Mermaid 1 MiB")
		end,
	})
end

return Window
