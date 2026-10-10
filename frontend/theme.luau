-- Presentation-only layout preference shared by host and window VMs.
local M = {}
M.options = {
	{ id = "current", title = "Current" },
	{ id = "aurora", title = "Aurora Glass" },
	{ id = "editorial", title = "Editorial Paper" },
	{ id = "amber", title = "Amber Ledger" },
}
local function valid(value: any): boolean
	for _, option in ipairs(M.options) do
		if value == option.id then return true end
	end
	return false
end
function M.read(): string
	local value = tern.kv.get("layout_theme")
	return valid(value) and value or "current"
end
function M.set(value: string): boolean
	if not valid(value) then return false end
	tern.kv.set("layout_theme", value)
	return true
end
function M.apply(state: any, value: string): boolean
	if not valid(value) or state.layout_theme == value then return false end
	state.layout_theme = value
	return true
end
function M.refresh(state: any): boolean
	return M.apply(state, M.read())
end
function M.event(state: any, ev: any): (boolean, boolean)
	if ev.id ~= "main.theme" or type(ev.act) ~= "string" then return false, false end
	local value = ev.act:match("^set_theme=(.+)$")
	if not value then return false, false end
	return true, M.apply(state, value)
end
function M.regions(regions: any, state: any, cx: any): any
	local id = valid(state.layout_theme) and state.layout_theme or "current"
	local role = "yazi.theme." .. id
	if id ~= "current" and (cx.cols or 120) < 120 then role ..= ".narrow" end
	for _, region in pairs(regions) do
		region.p = region.p or {}
		region.p.role = role
	end
	return regions
end
return M
