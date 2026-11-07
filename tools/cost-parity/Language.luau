-- Language-level Luau for the parity bench (tools/cost-parity): pure Luau loops, no engine calls. parity.py loads this source
-- twice, as is and with a `--!native` first line, so each workload is timed interpreted and natively compiled the way a
-- game module would be. Each entry is { N, function(N) }: N iterations a round; the function returns something so
-- nothing is optimised away.
local M = {}
local W = {}
M.W = W
M.Order = {}

local function add(Name, N, Fn)
	W[Name] = { N, Fn }
	table.insert(M.Order, Name)
end

add("arith.int", 1000000, function(N)
	local s = 0
	for i = 1, N do
		s = (s + i * 7) % 1000003
	end
	return s
end)

add("arith.float", 1000000, function(N)
	local s = 0.5
	for i = 1, N do
		s = s * 0.999 + i * 0.25
	end
	return s
end)

add("arith.mathlib", 400000, function(N)
	local s = 0
	for i = 1, N do
		s += math.floor(i * 0.5) + math.abs(-i) + math.max(i, 3)
	end
	return s
end)

add("arith.trig", 400000, function(N)
	local s = 0
	for i = 1, N do
		s += math.sin(i * 0.01) * math.cos(i * 0.02)
	end
	return s
end)

-- the number classifiers (Luau 0.701) on a value that changes every step
add("arith.isnan", 1000000, function(N)
	local s = 0
	for i = 1, N do
		if math.isnan(i * 0.5) then
			s += 1
		end
	end
	return s
end)

add("arith.isinf", 1000000, function(N)
	local s = 0
	for i = 1, N do
		if math.isinf(i * 0.5) then
			s += 1
		end
	end
	return s
end)

add("arith.isfinite", 1000000, function(N)
	local s = 0
	for i = 1, N do
		if math.isfinite(i * 0.5) then
			s += 1
		end
	end
	return s
end)

add("table.array_rw", 1000000, function(N)
	local t = table.create(256, 0)
	local s = 0
	for i = 1, N do
		local k = (i % 256) + 1
		t[k] = t[k] + i
		s += t[k]
	end
	return s
end)

add("table.field_rw", 1000000, function(N)
	local o = { x = 1, y = 2, z = 3 }
	for i = 1, N do
		o.x = o.y + i
		o.y = o.z - o.x
	end
	return o.x
end)

add("table.hash_dynamic", 400000, function(N)
	local keys = {}
	for i = 1, 64 do
		keys[i] = "key" .. i
	end
	local t = {}
	for i = 1, 64 do
		t[keys[i]] = i
	end
	local s = 0
	for i = 1, N do
		local k = keys[(i % 64) + 1]
		t[k] += 1
		s += t[k]
	end
	return s
end)

add("table.insert_clear", 200000, function(N)
	local t = {}
	local s = 0
	for i = 1, N do
		t[#t + 1] = i
		if #t == 64 then
			s += #t
			table.clear(t)
		end
	end
	return s
end)

local function addTwo(a, b)
	return a + b
end
add("call.local", 1000000, function(N)
	local s = 0
	for i = 1, N do
		s = addTwo(s, i)
	end
	return s
end)

local Object = {}
Object.__index = Object
function Object.new(v)
	return setmetatable({ v = v }, Object)
end
function Object:get(i)
	return self.v + i
end
add("call.method", 1000000, function(N)
	local o = Object.new(3)
	local s = 0
	for i = 1, N do
		s = o:get(i) - s
	end
	return s
end)

add("call.closure", 400000, function(N)
	local s = 0
	for i = 1, N do
		local f = function()
			return i
		end
		s += f()
	end
	return s
end)

add("call.vararg", 300000, function(N)
	local function count(...)
		return select("#", ...)
	end
	local s = 0
	for i = 1, N do
		s += count(i, i, i)
	end
	return s
end)

add("call.pcall", 300000, function(N)
	local s = 0
	for i = 1, N do
		local ok, v = pcall(addTwo, i, 1)
		if ok then
			s += v
		end
	end
	return s
end)

local function fib(n)
	if n < 2 then
		return n
	end
	return fib(n - 1) + fib(n - 2)
end
add("call.recursive", 20, function(N)
	local s = 0
	for _ = 1, N do
		s += fib(20)
	end
	return s
end)

add("iter.ipairs", 4000, function(N)
	local t = table.create(100, 1)
	local s = 0
	for _ = 1, N do
		for _, v in ipairs(t) do
			s += v
		end
	end
	return s
end)

add("iter.pairs", 4000, function(N)
	local t = {}
	for i = 1, 100 do
		t["k" .. i] = i
	end
	local s = 0
	for _ = 1, N do
		for _, v in pairs(t) do
			s += v
		end
	end
	return s
end)

add("iter.generalized", 4000, function(N)
	local t = table.create(100, 1)
	local s = 0
	for _ = 1, N do
		for _, v in t do
			s += v
		end
	end
	return s
end)

add("branch.if_chain", 1000000, function(N)
	local s = 0
	for i = 1, N do
		local r = i % 5
		if r == 0 then
			s += 1
		elseif r == 1 then
			s -= 1
		elseif r == 2 then
			s += 2
		elseif r == 3 then
			s -= 2
		else
			s += 3
		end
	end
	return s
end)

add("string.concat", 200000, function(N)
	local n = 0
	for i = 1, N do
		local s = "a" .. (i % 10)
		n += #s
	end
	return n
end)

add("string.format", 100000, function(N)
	local n = 0
	for i = 1, N do
		n += #string.format("%d:%s", i, "x")
	end
	return n
end)

add("string.tostring", 200000, function(N)
	local n = 0
	for i = 1, N do
		n += #tostring(i % 1000)
	end
	return n
end)

add("string.byte_sub", 400000, function(N)
	local text = "the quick brown fox jumps over the lazy dog"
	local n = 0
	for i = 1, N do
		local k = (i % 40) + 1
		n += string.byte(text, k) + #string.sub(text, k, k + 2)
	end
	return n
end)

add("vector.arith", 1000000, function(N)
	local v = Vector3.new(1, 2, 3)
	local acc = Vector3.zero
	for i = 1, N do
		acc = acc + v * (i % 7)
	end
	return acc.X
end)

add("vector.members", 1000000, function(N)
	-- (a vector the compiler cannot fold: a newer compiler folds member reads of a constant vector away)
	local v = Vector3.new(1, 2, N % 3)
	local s = 0
	for _ = 1, N do
		s += v.X + v.Y * v.Z
	end
	return s
end)

add("vector.magnitude", 1000000, function(N)
	local v = Vector3.new(1, 2, 3)
	local s = 0
	for i = 1, N do
		s += (v * i).Magnitude
	end
	return s
end)

add("cframe.mul", 200000, function(N)
	local a = CFrame.new(1, 2, 3) * CFrame.Angles(0.1, 0.2, 0.3)
	local c = CFrame.identity
	for _ = 1, N do
		c = c * a
	end
	return c.X
end)

add("cframe.point", 400000, function(N)
	local a = CFrame.new(1, 2, 3) * CFrame.Angles(0.1, 0.2, 0.3)
	local v = Vector3.new(1, 0, 0)
	local s = 0
	for _ = 1, N do
		s += (a * v).X
	end
	return s
end)

add("buffer.rw", 1000000, function(N)
	local b = buffer.create(1024)
	local s = 0
	for i = 1, N do
		local o = (i % 256) * 4
		buffer.writeu32(b, o, i)
		s += buffer.readu32(b, o)
	end
	return s
end)

add("alloc.table", 200000, function(N)
	local keep
	for i = 1, N do
		keep = { i, i + 1, i + 2 }
	end
	return keep[1]
end)

add("alloc.record", 200000, function(N)
	local keep
	for i = 1, N do
		keep = { x = i, y = i, z = i, w = i }
	end
	return keep.x
end)

-- a small insertion sort over a fixed array: comparisons and swaps in plain Luau
add("algo.sort", 2000, function(N)
	local base = table.create(64, 0)
	for i = 1, 64 do
		base[i] = (i * 37) % 64
	end
	local s = 0
	for _ = 1, N do
		local t = table.clone(base)
		for i = 2, #t do
			local v = t[i]
			local j = i - 1
			while j >= 1 and t[j] > v do
				t[j + 1] = t[j]
				j -= 1
			end
			t[j + 1] = v
		end
		s += t[1]
	end
	return s
end)

-- The optimisation level the module was compiled at: at O2 a small local function is inlined, so debug.info(1, "n")
-- inside it names its caller
-- What a script's environment gives it while getfenv / setfenv have not made it unsafe: its imports resolved when it loads (a
-- global read is a constant's load) and builtins called through the VM's fast path. The "unsafe" variant (parity.py) runs the
-- rows M.Unsafe names in a module that called getfenv first.
add("env.import", 1000000, function(N)
	local s = 0
	for _ = 1, N do
		local g, v = game, Vector3.new
		if g and v then
			s += 1
		end
	end
	return s
end)
add("env.typeof", 1000000, function(N)
	local s = 0
	for i = 1, N do
		if typeof(i) == "number" then
			s += 1
		end
	end
	return s
end)
add("env.type", 1000000, function(N)
	local s = 0
	for i = 1, N do
		if type(i) == "number" then
			s += 1
		end
	end
	return s
end)
M.Unsafe = { "env.import", "env.typeof", "env.type", "arith.mathlib", "arith.isnan", "arith.isinf", "arith.isfinite", "call.local", "call.method" }

local function inner()
	return debug.info(1, "n")
end
function M.CompiledName()
	return inner()
end

return M
