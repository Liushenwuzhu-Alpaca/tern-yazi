-- The palette entry and Ctrl+Alt+Y: split the focused pane and run `yazi` there. The
-- tern-yazi launcher behind `yazi` draws the browser on that pane's program surface, and the
-- pane closes when Yazi quits.
local function open(cx)
	local focused = cx.session:focused()
	if not focused then
		cx.layout:new_tab({ command = "yazi" })
		return
	end
	local info = cx.session:pane(focused)
	local cwd = info and info.cwd ~= "" and info.cwd or nil
	cx.layout:split(focused, "right", { cwd = cwd, command = "yazi" })
end

tern.command({
	id = "open",
	title = "Yazi: Open File Browser",
	group = "Yazi",
	keys = { "ctrl+alt+y" },
	run = open,
})
