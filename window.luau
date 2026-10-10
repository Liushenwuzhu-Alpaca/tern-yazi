-- Routes exist immediately; module compilation is spread across window budgets.
local inbox = require("./backend/inbox")
local control_path = require("./shared/control_path")
local handlers, pending = {}, {}
local ready = false

local function dispatch(req, cx)
	if not control_path.parse(req.path, inbox.state_dir()) then return nil end
	if not ready then
		pending[req.path .. ":" .. tostring(req.pane)] = req
		return { handled = true }
	end
	for _, handler in ipairs(handlers) do
		local decision = handler(req, cx)
		if decision then return decision end
	end
	return nil
end

tern.route.open(dispatch)
tern.route.link(function(link, cx)
	if type(link.pane) ~= "number" then return nil end
	return dispatch({ path = link.url, pane = link.pane, origin = "link" }, cx)
end)

-- Requires are cached per window VM; reload cancels all pending callbacks.
local modules = {
	"./backend/preview_policy",
	"./backend/window",
	"./frontend/theme",
	"./frontend/window",
}
local index = 1
local function initialize(cx)
	if index <= #modules then
		require(modules[index])
		index += 1
		tern.timer(0, initialize)
		return
	end
	require("./frontend/window").register(function(handler)
		table.insert(handlers, handler)
	end)
	ready = true
	local queued = pending
	pending = {}
	for _, req in pairs(queued) do dispatch(req, cx) end
end
tern.timer(0, initialize)
