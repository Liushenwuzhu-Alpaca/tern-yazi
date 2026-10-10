-- Native window presentation and registration. Protocol decisions stay backend-owned.
local backend = require('../backend/window')
local theme = require('./theme')
local BLOCK_KIND = "tern-yazi.companion"
local companion = { pane = nil }
local Window = {}

local function send_event(cx, report)
	if report and cx.session:pane(report.pane) then cx.session:event(report.pane, report.event) end
end
local function poll_pane(cx, pane)
	cx.session:event(pane, { ev = "action", id = "main.poll", act = "poll" })
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
local function restore_slot(cx, record, slot)
	if not slot or record.owner_pane == record.pane then return true end
	local owner = cx.session:pane(record.owner_pane)
	if not owner then return true end
	local focused = cx.session:focused()
	local native = cx.session:pane(record.pane)
	local in_slot = native and cx.session:tab_of(record.pane) == slot.tab
	local zoomed = slot.was_zoomed
	if in_slot then zoomed = pane_zoomed(cx, record.pane) end
	local target
	if slot.kind == "floating" then
		-- Float/unpark targets must be tiled leaves, not the focused float.
		target = backend.managed_float_target(slot, cx.session:layout(slot.tab))
	else
		target = in_slot and record.pane or nil
		if not target then
			for _, pane in ipairs(slot.anchors or {}) do
				if cx.session:tab_of(pane) == slot.tab then target = pane; break end
			end
		end
	end
	local ok, err = true, nil
	if owner.parked then
		if target then
			-- unpark is current-tab-relative. Select only this recorded source tab.
			cx.layout:focus(target)
			ok, err = cx.layout:unpark(record.owner_pane, { focus = false })
			if ok and slot.kind == "floating" then
				ok, err = cx.layout:float(record.owner_pane, target, slot.corner, { focus = false })
				if not ok then cx.layout:park(record.owner_pane) end
			elseif ok and not in_slot then
				ok = cx.layout:move(record.owner_pane, target, slot.dir or "right", { focus = false })
			end
		else
			-- Raw close may already have destroyed the last-pane tab. Restore in a
			-- dedicated tab, never beside an unrelated currently focused client.
			local tab
			tab, err = cx.layout:move_to_new_tab(record.owner_pane, slot.session, { focus = false })
			ok = tab ~= nil
			if ok then
				cx.layout:name_tab(tab, slot.name)
				cx.layout:color_tab(tab, slot.color)
				cx.layout:order_tab(tab, slot.position or 1)
			end
		end
	end
	if not ok then
		tern.log.warn("Yazi shell restoration failed", tostring(err or "layout refused restoration"))
		if focused and cx.session:pane(focused) then cx.layout:focus(focused) end
		return false
	end
	if native then
		-- Collapse only this temporary pair, or remove only this native float.
		-- The actual shell now owns the original presentation again.
		ok, err = cx.layout:park(record.pane)
		if not ok then
			tern.log.warn("Yazi return failed", tostring(err))
			cx.layout:park(record.owner_pane)
			if focused and cx.session:pane(focused) then cx.layout:focus(focused) end
			return false
		end
	end
	zoom_pane(cx, record.owner_pane, zoomed == true)
	if focused and focused ~= record.pane and focused ~= record.owner_pane
		and cx.session:pane(focused) then cx.layout:focus(focused) end
	backend.forget_managed_slot(record)
	return true
end
local function companion_alive(cx): boolean
	return companion.pane ~= nil and cx.session:pane(companion.pane) ~= nil
end

-- No-argument creation owns its backend; explicit client arguments attach only.
local function open_companion(cx)
	if companion_alive(cx) then return end
	local pane = cx:new_block(BLOCK_KIND, nil, "beside", { focus = true })
	if not pane then tern.log.warn("new_block failed", BLOCK_KIND); return end
	companion.pane = pane
	poll_pane(cx, pane)
end
local function toggle(cx)
	local focused = cx.session:focused()
	local pane = companion_alive(cx) and companion.pane or nil
	local record = backend.focused_managed(focused, query(cx))
	if record then cx.layout:focus(record.pane); return end
	-- The first toggle after reload can precede registry discovery.
	local info = focused and cx.session:pane(focused)
	if info and info.block == BLOCK_KIND then return end
	if not pane then open_companion(cx); return end
	cx.layout:focus(pane)
end
local function launch_managed(cx, record)
	if record.owner_kind == "standalone" then return end
	local plan = backend.managed_launch_plan(record)
	local focused = cx.session:focused()
	local tab = cx.session:tab_of(plan.owner_pane)
	local layout = tab and cx.session:layout(tab)
	if not layout then return end
	local slot = backend.managed_slot_plan(record, layout)
	if not slot then
		cx:toast("error", "Cannot open native Yazi", "The launching pane has no restorable visual slot")
		return
	end
	slot.tab, slot.was_zoomed, slot.owner_pane = tab, layout.zoomed, plan.owner_pane
	for _, info in ipairs(cx.session:tabs()) do
		if info.id == tab then
			slot.session, slot.name, slot.color, slot.position = info.session, info.name, info.color, info.position
			break
		end
	end
	-- Do not depend on new_block beside a float: use its actual tiled owner.
	cx.layout:focus(slot.kind == "floating" and slot.over or plan.owner_pane)
	local pane = cx:new_block(BLOCK_KIND, plan.args, "beside", { focus = true })
	if not pane then
		if focused then cx.layout:focus(focused) end
		return
	end
	backend.managed_launched(record, pane)
	slot.pane = pane
	backend.save_managed_slot(record, slot)
	local ok, err = cx.layout:park(plan.owner_pane)
	if not ok then
		-- A failed park is not a usable in-place launch. Keep the original shell.
		cx.layout:close(pane)
		cx:toast("error", "Cannot open native Yazi", tostring(err or "Cannot preserve the launching shell"))
	elseif slot.kind == "floating" then
		ok, err = cx.layout:float(pane, slot.over, slot.corner, { focus = true })
		if not ok then
			-- Restore the actual shell before abandoning a failed presentation.
			restore_slot(cx, record, slot)
			cx.layout:close(pane)
			cx:toast("error", "Cannot open native Yazi", tostring(err or "Cannot preserve the floating slot"))
		else zoom_pane(cx, pane, slot.was_zoomed == true) end
	else zoom_pane(cx, pane, slot.was_zoomed == true) end
	if focused and focused ~= plan.owner_pane and cx.session:pane(focused) then cx.layout:focus(focused) end
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
		if candidate.launch then launch_managed(cx, record) end
		if not backend.managed_active(candidate, metadata) then continue end
		if not record.pane then launch_managed(cx, record) end
		if not record.pane then continue end
		local slot = backend.managed_slot(record)
		if slot and cx.session:tab_of(record.pane) then
			local zoomed = pane_zoomed(cx, record.pane)
			if slot.was_zoomed ~= zoomed then slot.was_zoomed = zoomed; backend.save_managed_slot(record, slot) end
		end
		local lease = backend.managed_lease(record)
		if lease == "failed" then
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

function Window.register(control_route)
	local function return_managed(result, cx)
		if not result then return nil end
		if result.record then
			local ok = restore_slot(cx, result.record, result.slot)
			cx.session:event(result.record.pane, { ev = "action", id = "main.poll", act = "managed_return", ok = ok })
		end
		if not result.record and result.pane and cx.session:pane(result.pane) then
			cx.session:event(result.pane, { ev = "action", id = "main.poll", act = "managed_return", ok = false })
		end
		return { handled = true }
	end
	-- Host q/offline-Esc sends a token-bound plain route before BlockCx.exit.
	control_route(function(req, cx)
		return return_managed(backend.managed_close_request(req, query(cx)), cx)
	end)
	-- Launcher file-open wakes idle windows without depending on the timer.
	control_route(function(req, cx)
		local result = backend.managed_request(req, query(cx))
		if not result then return nil end
		if result.wake then poke(cx)
		elseif not result.foreign then
			cx:toast("error", "Cannot open managed Yazi", "The launching pane or managed session is missing")
		end
		return { handled = true }
	end)
	tern.on("window_start", function(cx) poke(cx) end)
	tern.on("pane_closed", function(ev, cx)
		if companion.pane == ev.pane then companion.pane = nil end
		for _, plan in ipairs(backend.managed_closed(ev.pane)) do
			if plan.owner_closed then
				backend.forget_managed_slot(plan.record)
				if plan.record.pane ~= ev.pane and cx.session:pane(plan.record.pane) then
					cx.layout:close(plan.record.pane)
				end
			else restore_slot(cx, plan.record, plan.slot) end
		end
	end)

	control_route(function(req, cx)
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

	control_route(function(req, cx)
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

	control_route(function(req, cx)
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

	for _, option in ipairs(theme.options) do
		local id = option.id
		tern.command({
			id = "theme_" .. id,
			title = "Yazi: Layout - " .. option.title,
			group = "Yazi",
			run = function(cx)
				if not theme.set(id) then return end
				for _, pane in ipairs(cx.session:panes()) do
					if pane.block == BLOCK_KIND then
						cx.session:event(pane.pane, { ev = "action", id = "main.theme", act = "set_theme=" .. id })
					end
				end
			end,
		})
	end

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
