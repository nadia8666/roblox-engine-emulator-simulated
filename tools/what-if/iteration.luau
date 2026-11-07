-- ipairs or a numeric for: walking an array of n numbers.
-- Run: add the array up, four ways.
-- ree profile try tools/what-if/iteration.luau

local function numbers(n)
	local list = table.create(n)
	for i = 1, n do
		list[i] = (i * 13) % 101
	end
	return list
end

return {
	title = "ipairs or a numeric for",
	about = "an array of n numbers added up: ipairs, a numeric for over #t, generalized iteration (for _, v in t) and pairs",
	sizes = { 10, 100, 1000, 10000 },
	setup = numbers,
	variants = {
		{
			name = "ipairs",
			run = function(list)
				local total = 0
				for _, value in ipairs(list) do
					total += value
				end
				return total
			end,
		},
		{
			name = "numeric for",
			run = function(list)
				local total = 0
				for i = 1, #list do
					total += list[i]
				end
				return total
			end,
		},
		{
			name = "for _, v in t",
			run = function(list)
				local total = 0
				for _, value in list do
					total += value
				end
				return total
			end,
		},
		{
			name = "pairs",
			run = function(list)
				local total = 0
				for _, value in pairs(list) do
					total += value
				end
				return total
			end,
		},
	},
	check = true,
}
