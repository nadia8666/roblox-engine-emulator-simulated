-- A new table every frame or one reused: each frame lists the items that pass a test.
-- Run: 30 frames, one call a frame (frames = 30); figures are per frame.
-- ree profile try tools/what-if/reuse-table.luau

local function items(n)
	local list = table.create(n)
	for i = 1, n do
		list[i] = { id = i, score = (i * 37) % 100 }
	end
	return { items = list, buffer = {}, frame = 0 }
end

local function passes(item, frame)
	return (item.score + frame) % 100 >= 50
end

return {
	title = "a new table per frame or one reused",
	about = "each frame collects the items that pass a test into a list: a new {} each frame, a new table.create(n), or one table cleared and refilled",
	sizes = { 10, 100, 1000, 10000 },
	frames = 30,
	setup = items,
	variants = {
		{
			name = "new table",
			run = function(data)
				data.frame += 1
				local chosen = {}
				for _, item in data.items do
					if passes(item, data.frame) then
						table.insert(chosen, item)
					end
				end
				data.chosen = chosen
				return #chosen
			end,
		},
		{
			name = "new table.create(n)",
			run = function(data)
				data.frame += 1
				local chosen = table.create(#data.items)
				for _, item in data.items do
					if passes(item, data.frame) then
						table.insert(chosen, item)
					end
				end
				data.chosen = chosen
				return #chosen
			end,
		},
		{
			name = "reused + table.clear",
			run = function(data)
				data.frame += 1
				local chosen = data.buffer
				table.clear(chosen)
				for _, item in data.items do
					if passes(item, data.frame) then
						table.insert(chosen, item)
					end
				end
				data.chosen = chosen
				return #chosen
			end,
		},
	},
	check = true,
}
