-- One preview byte-limit policy for reads and the native settings command.
local M = {
	limits = {
		image_bytes = { default = 16 * 1024 * 1024, max = 16 * 1024 * 1024 },
		markdown_bytes = { default = 1024 * 1024, max = 4 * 1024 * 1024 },
		code_bytes = { default = 1024 * 1024, max = 4 * 1024 * 1024 },
		mermaid_bytes = { default = 256 * 1024, max = 1024 * 1024 },
	},
}

function M.defaults(): { [string]: number }
	return {
		image_bytes = M.limits.image_bytes.default,
		markdown_bytes = M.limits.markdown_bytes.default,
		code_bytes = M.limits.code_bytes.default,
		mermaid_bytes = M.limits.mermaid_bytes.default,
	}
end

return M
