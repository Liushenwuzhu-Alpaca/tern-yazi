-- Presentation-only layout preference shared by host and window VMs.
-- Each theme is `themes/<id>.css` (listed in plugin.toml `styles`) plus one entry here.
-- Optional structural flags: `narrow` (role gains `.narrow` below 120 columns),
-- `plain_parent` (no parent-column icons), `ordinals` (row numbers), `tray` (selection tray in the dock).
local M = {}
M.options = {
	{ id = "current", title = "Current" },
	{ id = "aurora", title = "Aurora Glass", narrow = true },
	{ id = "editorial", title = "Editorial Paper", narrow = true, plain_parent = true },
	{ id = "amber", title = "Amber Ledger", narrow = true, ordinals = true, tray = true },
}
local by_id = {}
for _, option in ipairs(M.options) do by_id[option.id] = option end
local function valid(value: any): boolean
	return by_id[value] ~= nil
end
-- The option record for a theme id; unknown ids fall back to Current.
function M.get(value: string?): any
	return by_id[value] or by_id.current
end
function M.read(): string
	local env_theme = tern.getenv("YAZI_THEME")
	if valid(env_theme) then return env_theme end
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
	local option = M.get(state.layout_theme)
	local role = "yazi.theme." .. option.id
	if option.narrow and (cx.cols or 120) < 120 then role ..= ".narrow" end
	for _, region in pairs(regions) do
		region.p = region.p or {}
		region.p.role = role
	end
	return regions
end
return M
