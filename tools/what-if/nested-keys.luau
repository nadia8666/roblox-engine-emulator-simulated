-- t[k][v] or t[v][k]: a value for each of a few kinds (k) and many items (v), nested either way.
-- Run: build the index from a list of (item, kind, value) rows, then read every value back.
-- ree profile try tools/what-if/nested-keys.luau

local KINDS = { "speed", "range", "damage", "armor" }

local function rows(n)
	local list = table.create(n * #KINDS)
	for item = 1, n do
		for index, kind in KINDS do
			table.insert(list, { item = item, kind = kind, value = item * 10 + index })
		end
	end
	return { rows = list, n = n }
end

return {
	title = "t[k][v] or t[v][k]",
	about = "4 kinds by n items: by kind then item (a few big tables) or by item then kind (many small ones)",
	sizes = { 10, 100, 1000, 10000 },
	setup = rows,
	variants = {
		{
			name = "t[k][v]",
			run = function(data)
				local index = {}
				for _, row in data.rows do
					local byItem = index[row.kind]
					if byItem == nil then
						byItem = {}
						index[row.kind] = byItem
					end
					byItem[row.item] = row.value
				end
				local total = 0
				for item = 1, data.n do
					for _, kind in KINDS do
						total += index[kind][item]
					end
				end
				return { index = index, total = total }
			end,
		},
		{
			name = "t[v][k]",
			run = function(data)
				local index = {}
				for _, row in data.rows do
					local byKind = index[row.item]
					if byKind == nil then
						byKind = {}
						index[row.item] = byKind
					end
					byKind[row.kind] = row.value
				end
				local total = 0
				for item = 1, data.n do
					local byKind = index[item]
					for _, kind in KINDS do
						total += byKind[kind]
					end
				end
				return { index = index, total = total }
			end,
		},
	},
	-- (the indexes differ in shape by design: the same values read back is the check)
	check = function(a, b)
		return a.total == b.total, `totals {a.total} and {b.total}`
	end,
}
