-- Host registration and presentation adapter for the companion backend.
local session = require('../backend/session')
local actions = require('../backend/actions')
local model = require('../backend/model')
local preview = require('../backend/preview')
local path = require('../shared/path')
local input = require('./input')
local view = require('./view')
local theme = require('./theme')
local M = {}
local contexts, launch_contexts = {}, {}


local function cancel_hydration(state: any)
	state.hydration_ticket = nil
	if state.hydration_timer then state.hydration_timer:cancel(); state.hydration_timer = nil end
end

local function sync_hydration(state: any)
	if state.closing or state.return_pending or not contexts[state] or not view.hydration_pending(state) then
		cancel_hydration(state)
		return
	end
	if state.hydration_timer then state.hydration_timer:cancel() end
	local ticket = {}
	state.hydration_ticket = ticket
	state.hydration_timer = tern.timer(150, function()
		if state.hydration_ticket ~= ticket then return end
		state.hydration_timer, state.hydration_ticket = nil, nil
		if state.closing or state.return_pending or not contexts[state] then return end
		if view.hydrate(state) then state.on_change(true) end
		sync_hydration(state)
	end)
end

local function render(state: any, cx: any, yz: any, prepared: any?, data: any?): any
	local regions = view.render(state, cx, yz, prepared, data)
	sync_hydration(state)
	return regions
end

local function dimensions(state: any, cx: any): any
	local pane = state.pane_metadata
	pane.rows, pane.cols = cx.rows, cx.cols
	return pane
end

local function probe(state: any, yz: any)
	if not state.token and yz and not state.sending and tern.now() >= (state.next_probe or 0) then
		state.next_probe = tern.now() + 2000
		actions.request(state, yz, { op = "ping" })
	end
end

local function present(state: any, ev: any)
	local cx = contexts[state]
	if ev.kind == "toast" then
		cx:toast(ev.level, ev.title, ev.detail)
	elseif ev.kind == "open" then
		cx:open(ev.path)
	elseif ev.kind == "restore_command" then
		input.restore_command(state, ev.block, ev.query)
	elseif ev.kind == "reset_input" then
		input.reset(state, ev.clear_trash)
	end
end

local function dispatch_key(state: any, ev: any, cx: any, yz: any): boolean
	if state.snapshot_error or (state.live and not yz) then
		local name = (ev.name or ""):lower()
		if (name == "q" or name == "escape") and not ev.ctrl and not ev.alt and not ev.meta then state.close(cx)
		else cx:toast("error", "Yazi snapshot unavailable", state.snapshot_error or "Waiting for a consistent Yazi snapshot") end
		return true
	end
	return input.key(state, ev, cx, yz)
end

local function key(state: any, ev: any, cx: any): boolean
	local yz = session.read_latest_state(state)
	input.sync_hover(state, yz)
	return dispatch_key(state, ev, cx, yz)
end

local function register()
	-- Shell effects wake the owning window; window timers alone can sleep idle.
	tern.on("command_started", function(ev, cx) launch_contexts[ev.pane] = cx end)
	tern.on("pane_exited", function(ev) launch_contexts[ev.pane] = nil end)
	session.start_discovery(function(pane) return launch_contexts[pane] ~= nil end, function(pane, path)
		launch_contexts[pane]:open(path)
	end)

	tern.block.define("companion", {
		features = { "edit" },
		init = function(cx, args, saved)
			local state = session.new(args, saved, cx.pane)
			state.source_cwd = state.source_cwd or cx.cwd
			theme.refresh(state)
			input.init(state)
			preview.init(state)
			contexts[state] = cx
			state.pane_metadata = { pane = cx.pane, rows = cx.rows, cols = cx.cols }
			state.pane_list = tern.pane.list
			state.on_event = function(ev) present(state, ev) end
			state.on_change = function(sampled)
				if not sampled then session.read_latest_state(state) end
				contexts[state]:render()
			end
			state.on_close = function()
				cancel_hydration(state)
				actions.clear_handoffs(state)
				contexts[state] = nil
			end
			state.close = function(context)
				if state.return_pending then return end
				input.close(state)
				cancel_hydration(state)
				local route = session.close_route(state)
				if route then
					state.return_pending = true
					context:open(route)
				else session.close(state); context:exit(0) end
			end
			state.preview_config = preview.config_stamp
			state.on_poll = function(yz)
				local context = contexts[state]
				if theme.refresh(state) then context:render() end
				actions.poll_native_shells(state)
				local identity = tostring(state.cid) .. ":" .. tostring(state.token) .. ":" .. tostring(state.startup_attempted)
					.. ":" .. tostring(state.startup_error) .. ":" .. tostring(state.lease_nonce)
				if state.saved_identity ~= identity then state.saved_identity = identity; context:save() end
				if state.token and state.lease_nonce then
					session.managed_poll(state, { cid = state.cid, token = state.token, nonce = state.lease_nonce },
						dimensions(state, context))
				elseif state.owner_kind ~= "standalone" then probe(state, yz) end
			end
			session.install(state)
			return state
		end,

		view = function(state, cx)
			contexts[state] = cx
			local yz = session.latest_state(state)
			if not yz or not state.live then return render(state, cx, yz) end
			input.sync_hover(state, yz)
			preview.sync(state, yz)
			local prepared = model.presentation(state, yz)
			if state.optimistic_cwd and yz.cwd == state.optimistic_cwd then state.optimistic_cwd = nil end
			local data
			if prepared.active_name and prepared.is_active_dir == false then
				data = preview.prepare(state, prepared.active_full_url)
				-- Uploading native blobs is presentation, not backend file IO.
				if data.tag == "image" and #data.content > 0 then
					local ok, blob = pcall(function() return cx:blob(data.content, data.mime) end)
					if ok then data.blob = blob end
				end
			end
			return render(state, cx, yz, prepared, data)
		end,

		title = function(state)
			local yz = session.latest_state(state)
			if yz and yz.cwd then return "Companion (" .. path.basename(yz.cwd) .. ")" end
			return "Yazi Companion"
		end,

		event = function(state, ev, cx)
			local handled, changed = theme.event(state, ev)
			if handled then
				if changed then cx:render() end
				return
			end
			if theme.refresh(state) then cx:render() end
			local yz = session.read_latest_state(state)
			input.sync_hover(state, yz)
			local act = ev.act
			if act == "managed_return" and state.return_pending then
				state.return_pending = nil
				if ev.ok == false then cx:toast("error", "Cannot return to shell", "The original pane could not be restored"); return end
				session.close(state)
				cx:exit(0)
				return
			end
			if act == "shortcut" and type(ev.key) == "table" then dispatch_key(state, ev.key, cx, yz); return end
			if act == "managed_poll" then
				session.managed_poll(state, ev, dimensions(state, cx)); return
			end
			if act == "shell_handoff_check" or act == "shell_launched" or act == "shell_failed" then
				actions.shell_handoff_event(state, ev); return
			end
			if act == "files_handoff_check" or act == "files_opened" or act == "files_failed" then
				actions.open_selected(state, nil, ev); return
			end
			if act == "poll" then probe(state, yz); return end
			if state.snapshot_error or (state.live and not yz) then
				cx:toast("error", "Yazi snapshot unavailable", state.snapshot_error or "Waiting for a consistent Yazi snapshot")
				return
			end
			input.event(state, ev, cx, yz)
		end,

		key = key,
		save = session.save,
	})
end

M.register = register
return M
