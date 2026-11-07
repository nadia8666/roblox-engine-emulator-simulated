-- Closures or methods: n small objects with state and two operations, made and used.
-- Run: make n counters, add to each 4 times, read each, keep them (the heap shows what one costs).
-- ree profile try tools/what-if/closures-or-methods.luau

local Counter = {}
Counter.__index = Counter

function Counter.new(start)
	return setmetatable({ count = start }, Counter)
end

function Counter:Add(amount)
	self.count += amount
end

function Counter:Get()
	return self.count
end

local function closureCounter(start)
	local count = start
	return {
		Add = function(amount)
			count += amount
		end,
		Get = function()
			return count
		end,
	}
end

return {
	title = "closures or methods",
	about = "n counters: a table of two closures over a local, or a table with a metatable whose __index is the class",
	sizes = { 10, 100, 1000, 10000 },
	variants = {
		{
			name = "methods",
			run = function(n)
				local counters, total = table.create(n), 0
				for i = 1, n do
					local counter = Counter.new(i)
					for _ = 1, 4 do
						counter:Add(1)
					end
					total += counter:Get()
					counters[i] = counter
				end
				return { counters = counters, total = total }
			end,
		},
		{
			name = "closures",
			run = function(n)
				local counters, total = table.create(n), 0
				for i = 1, n do
					local counter = closureCounter(i)
					for _ = 1, 4 do
						counter.Add(1)
					end
					total += counter.Get()
					counters[i] = counter
				end
				return { counters = counters, total = total }
			end,
		},
	},
	check = function(a, b)
		return a.total == b.total, `totals {a.total} and {b.total}`
	end,
}
