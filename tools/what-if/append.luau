-- table.insert or t[#t+1]: appending n values to a new list.
-- Run: build the list four ways; the result is the list.
-- ree profile try tools/what-if/append.luau

return {
	title = "table.insert or t[#t+1]",
	about = "n values appended to a new list: table.insert, t[#t+1], a counted index, and a counted index into table.create(n)",
	sizes = { 10, 100, 1000, 10000 },
	variants = {
		{
			name = "table.insert",
			run = function(n)
				local list = {}
				for i = 1, n do
					table.insert(list, i * 2)
				end
				return list
			end,
		},
		{
			name = "t[#t+1]",
			run = function(n)
				local list = {}
				for i = 1, n do
					list[#list + 1] = i * 2
				end
				return list
			end,
		},
		{
			name = "counted index",
			run = function(n)
				local list = {}
				local count = 0
				for i = 1, n do
					count += 1
					list[count] = i * 2
				end
				return list
			end,
		},
		{
			name = "table.create + index",
			run = function(n)
				local list = table.create(n)
				for i = 1, n do
					list[i] = i * 2
				end
				return list
			end,
		},
	},
	check = true,
}
