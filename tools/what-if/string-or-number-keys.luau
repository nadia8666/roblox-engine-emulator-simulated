-- String keys or number keys: a lookup table of n entries by id, read 4n times.
-- Run: look up every id four times and add the values up.
-- ree profile try tools/what-if/string-or-number-keys.luau

local function order(n)
	-- (a fixed, scattered order of lookups)
	local ids = table.create(n * 4)
	local id = 1
	for i = 1, n * 4 do
		id = (id * 7919 + 13) % n + 1
		ids[i] = id
	end
	return ids
end

return {
	title = "string keys or number keys",
	about = "a table of n values by id, looked up 4n times: by the number itself, by a string id held, or by a string built for each lookup",
	sizes = { 10, 100, 1000, 10000 },
	variants = {
		{
			name = "number keys",
			setup = function(n)
				local values = table.create(n)
				for id = 1, n do
					values[id] = id * 3
				end
				return { values = values, keys = order(n) }
			end,
			run = function(data)
				local values, total = data.values, 0
				for _, id in data.keys do
					total += values[id]
				end
				return total
			end,
		},
		{
			name = "string keys",
			setup = function(n)
				local values = {}
				local names = table.create(n)
				for id = 1, n do
					names[id] = `item{id}`
					values[names[id]] = id * 3
				end
				local keys = order(n)
				for i, id in keys do
					keys[i] = names[id]
				end
				return { values = values, keys = keys }
			end,
			run = function(data)
				local values, total = data.values, 0
				for _, name in data.keys do
					total += values[name]
				end
				return total
			end,
		},
		{
			name = "string built per lookup",
			setup = function(n)
				local values = {}
				for id = 1, n do
					values[`item{id}`] = id * 3
				end
				return { values = values, keys = order(n) }
			end,
			run = function(data)
				local values, total = data.values, 0
				for _, id in data.keys do
					total += values["item" .. id]
				end
				return total
			end,
		},
	},
	check = true,
}
