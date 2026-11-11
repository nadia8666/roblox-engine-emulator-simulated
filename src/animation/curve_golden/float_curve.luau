-- FloatCurve: its members, their errors and edge cases, and GetValueAtTime for every interpolation mode. Runs
-- unchanged in Studio (the command bar or a plugin) and here; float_curve.txt is what Studio 0.747 returned
-- (2026-11-11).
local out = {}
local function S(v) if type(v) == "number" then return string.format("%.9g", v) elseif type(v) == "table" then local t = {} for i, x in v do t[#t+1] = S(x) end local n = 0 for _ in v do n += 1 end return "{" .. table.concat(t, ", ") .. "}" .. (n ~= #v and "#" .. n or "") else return tostring(v) end end
local function P(...) local t = table.pack(...) for i = 1, t.n do t[i] = S(t[i]) end table.insert(out, table.concat(t, " ")) end
local function try(f, ...) local r = table.pack(pcall(f, ...)) if r[1] then local t = {} for i = 2, r.n do t[#t+1] = S(r[i]) end return "ok(" .. table.concat(t, ", ") .. ")" else return "E(" .. tostring(r[2]) .. ")" end end
local L, C, K = Enum.KeyInterpolationMode.Linear, Enum.KeyInterpolationMode.Constant, Enum.KeyInterpolationMode.Cubic
local function FK(t, v, m, lt, rt) local k = FloatCurveKey.new(t, v, m) if lt then k.LeftTangent = lt end if rt then k.RightTangent = rt end return k end
local function curve(keys) local f = Instance.new("FloatCurve") f:SetKeys(keys) return f end
local function samples(f, from, to, n) local row = {} for i = 0, n do table.insert(row, S(f:GetValueAtTime(from + (to - from) * i / n))) end return table.concat(row, " ") end

-- Members
do
	local f = Instance.new("FloatCurve")
	P("empty", f.Length, try(f.GetValueAtTime, f, 0), try(f.GetKeys, f), try(f.GetKeyAtIndex, f, 1), try(f.GetKeyAtIndex, f, 0), try(f.GetKeyIndicesAtTime, f, 0), try(f.RemoveKeyAtIndex, f, 1, 1), try(f.RemoveKeyAtIndex, f, 1), try(f.RemoveKeyAtIndex, f))
	P("hidden", try(function() f.Length = 3 end), try(function() return f.ValuesAndTimes end), try(function() f.ValuesAndTimes = "" end))
	P("insert", try(f.InsertKey, f, FloatCurveKey.new(1, 10, L)), try(f.InsertKey, f, FloatCurveKey.new(0.5, 5, L)), try(f.InsertKey, f, FloatCurveKey.new(2, 20, C)), f.Length)
	P("insert-same-time", try(f.InsertKey, f, FloatCurveKey.new(1, 11, K)), f.Length, f:GetKeyAtIndex(2))
	P("insert-before", try(f.InsertKey, f, FloatCurveKey.new(-1, -5, L)), f.Length)
	P("insert-bad", try(f.InsertKey, f), try(f.InsertKey, f, 1), try(f.InsertKey, f, RotationCurveKey.new(0, CFrame.new(), L)), try(f.InsertKey, f, Vector3.zero), try(f.InsertKey, f, "s"), try(f.InsertKey, f, {}))
	P("insert-not-a-number", try(f.InsertKey, f, FloatCurveKey.new(0 / 0, 1, L)), try(f.InsertKey, f, FloatCurveKey.new(1 / 0, 1, L)), f.Length)
	P("keys", f:GetKeys())
	P("index", try(f.GetKeyAtIndex, f, 0), try(f.GetKeyAtIndex, f, 5), try(f.GetKeyAtIndex, f, 6), try(f.GetKeyAtIndex, f, "2"), try(f.GetKeyAtIndex, f, -1), try(f.GetKeyAtIndex, f), try(f.GetKeyAtIndex, f, {}), try(f.GetKeyAtIndex, f, "x"))
	local row = {}
	for _, i in { 0.4, 0.5, 0.6, 1.4, 1.5, 1.6, 2.5, 5.4, 5.5, -0.4, -0.5 } do table.insert(row, i .. "=" .. try(function() return f:GetKeyAtIndex(i).Value end)) end
	P("index-rounding", table.concat(row, " "))
	for _, t in { -2, -1, -0.5, 0.5, 0.75, 1, 1.5, 2, 3 } do P("at", t, try(f.GetKeyIndicesAtTime, f, t), try(f.GetValueAtTime, f, t)) end
	P("at-bad", try(f.GetValueAtTime, f), try(f.GetValueAtTime, f, "1"), try(f.GetValueAtTime, f, true), try(f.GetValueAtTime, f, {}), try(f.GetValueAtTime, f, "x"), try(f.GetKeyIndicesAtTime, f, 0 / 0), try(f.GetKeyIndicesAtTime, f), try(f.GetKeyIndicesAtTime, f, "x"))
	local a = f:GetKeyAtIndex(1) a.Value = 99 P("key-copy", f:GetKeyAtIndex(1).Value, a == f:GetKeyAtIndex(1))
	P("remove", try(f.RemoveKeyAtIndex, f, 2, 2), f.Length, f:GetKeys())
	P("remove-range", try(f.RemoveKeyAtIndex, f, 2, 10), f.Length, try(f.RemoveKeyAtIndex, f, 0, 1), try(f.RemoveKeyAtIndex, f, -1, 1), try(f.RemoveKeyAtIndex, f, 5, 1), try(f.RemoveKeyAtIndex, f, 1, 0), try(f.RemoveKeyAtIndex, f, 1, -1), try(f.RemoveKeyAtIndex, f, "x"), try(f.RemoveKeyAtIndex, f, 1, "x"), f.Length)
	local g = curve({ FK(1, 0, L), FK(2, 10, L), FK(3, 30, L), FK(4, 5, L) })
	P("remove-rounding", try(g.RemoveKeyAtIndex, g, 1.5, 1.5), g.Length, try(g.RemoveKeyAtIndex, g, 0.5, 1), try(g.RemoveKeyAtIndex, g, 1, -0.4), try(g.RemoveKeyAtIndex, g, 1, -0.5), g.Length)
	P("set", try(f.SetKeys, f, { FloatCurveKey.new(3, 1, L), FloatCurveKey.new(1, 2, L), FloatCurveKey.new(2, 3, L), FloatCurveKey.new(1, 4, L) }), f.Length, f:GetKeys())
	P("set-bad", try(f.SetKeys, f), try(f.SetKeys, f, 1), try(f.SetKeys, f, { 1, 2 }), try(f.SetKeys, f, { FloatCurveKey.new(0, 1, L), 5 }), f.Length, try(f.SetKeys, f, { a = FloatCurveKey.new(0, 1, L) }), f.Length, try(f.SetKeys, f, {}), f.Length)
	local changes = 0
	local c = Instance.new("FloatCurve") c.Changed:Connect(function() changes += 1 end)
	c:InsertKey(FloatCurveKey.new(0, 1, L)) c:SetKeys({ FloatCurveKey.new(0, 1, L) }) c:RemoveKeyAtIndex(1)
	P("changed", changes)
end

-- Key times: ticks of 1/2400 s, and how close SetKeys lets two keys be
do
	local f = Instance.new("FloatCurve")
	local bad = 0
	for k = 1, 6000 do
		f:SetKeys({ FloatCurveKey.new(k / 2400, 0, L) })
		if math.round(f:GetKeyAtIndex(1).Time * 2400) ~= k then bad += 1 end
	end
	local list = {}
	for _, t in { -0.5001, -0.0001, 0.0004, 0.00041, 0.00042, 0.99999, 0.9999999, 0.33333, 1e-7, 894784.8, 894784.9, 1e9 } do
		f:SetKeys({ FloatCurveKey.new(t, 0, L) }) table.insert(list, string.format("%.9g=>%.9g", t, f:GetKeyAtIndex(1).Time * 2400))
	end
	P("ticks", bad, table.concat(list, " "))
	P("quantized", curve({ FK(0.1234567, 0.1, L), FK(1 / 3, 1 / 3, L), FK(1.00001, 7, L), FK(1.00002, 8, L), FK(1.0002, 9, L), FK(1.0003, 10, L), FK(1000.12345, 11, L) }):GetKeys())
	P("near", curve({ FK(1, 7, L), FK(1.0003, 10, L) }):GetKeys(), curve({ FK(1, 7, L), FK(1.0005, 10, L) }):GetKeys(), curve({ FK(1.0003, 7, L) }):GetKeys())
	f = curve({ FK(1, 7, L) }) P("insert-near", f:InsertKey(FK(1.0003, 10, L)), f:GetKeys())
	P("small", curve({ FK(0.0004, 7, L), FK(0.0008, 8, L), FK(0.0012, 9, L), FK(0.0013, 9.5, L) }):GetKeys())
	P("same-times", curve({ FK(2, 7, L), FK(1, 8, L), FK(2, 9, K), FK(1, 10, C) }):GetKeys())
	P("run", curve({ FK(0, 1, L), FK(0.00021, 2, L), FK(0.00062, 3, L), FK(0.00063, 4, L), FK(0.00104, 5, L), FK(0.00105, 6, L) }):GetKeys())
	local gaps = { 0.9, 0.95, 0.99, 0.999, 1, 1.001, 1.01, 1.05 }
	for _, from in { 0, 1, 0.3 / 2400 } do
		local row = {}
		for _, gap in gaps do table.insert(row, gap .. "=" .. curve({ FK(from, 1, L), FK(from + gap / 2400, 2, L) }).Length) end
		P("gap", from, table.concat(row, " "))
	end
	for _, from in { 0, 7, 700 } do
		local keys = {}
		for k = 0, 200 do table.insert(keys, FK(from + k / 2400, k, L)) end
		f = curve(keys)
		P("a-tick-apart", from, f.Length, try(f.SetKeys, f, f:GetKeys()), f.Length)
	end
end

-- Which keys a time is between
do
	local one = curve({ FK(1, 5, L) })
	P("one", one:GetKeyIndicesAtTime(0), one:GetKeyIndicesAtTime(1), one:GetKeyIndicesAtTime(2), one:GetValueAtTime(0), one:GetValueAtTime(1), one:GetValueAtTime(2))
	local two = curve({ FK(1, 5, L), FK(2, 7, L) })
	P("two", two:GetKeyIndicesAtTime(0), two:GetKeyIndicesAtTime(1), two:GetKeyIndicesAtTime(1.5), two:GetKeyIndicesAtTime(2), two:GetKeyIndicesAtTime(3), two:GetValueAtTime(0), two:GetValueAtTime(3))
	P("between-ticks", two:GetValueAtTime(1.5), two:GetValueAtTime(1.5001), two:GetValueAtTime(1.5002), two:GetValueAtTime(1.5004), two:GetValueAtTime(1.99999), two:GetValueAtTime(0.99999))
	local three = curve({ FK(1, 0, L), FK(2, 10, L), FK(3, 30, L) })
	local row = {}
	for _, t in { 1.9997, 1.9998, 1.9999, 2, 2.0001, 2.0002, 0.9999, 2.9999, 3.0001 } do
		local i = three:GetKeyIndicesAtTime(t)
		table.insert(row, t .. "=" .. S(three:GetValueAtTime(t)) .. "{" .. i[1] .. "," .. i[2] .. "}")
	end
	P("near-key", table.concat(row, " "))
	local constant = curve({ FK(1, 0, C), FK(2, 10, C), FK(3, 30, L) })
	row = {}
	for _, t in { 1.9997, 1.9998, 2, 2.9998, 2.9999, 3 } do table.insert(row, t .. "=" .. S(constant:GetValueAtTime(t))) end
	P("near-key-constant", table.concat(row, " "))
	local cubic = curve({ FK(1, 0, K, nil, 3), FK(2, 10, K, 7, -4), FK(3, 30, K, 2, nil) })
	row = {}
	for _, t in { 1.9997, 1.9998, 2, 2.0001, 2.9998, 2.9999 } do table.insert(row, t .. "=" .. S(cubic:GetValueAtTime(t))) end
	P("near-key-cubic", table.concat(row, " "))
end

-- Constant and Linear
do
	P("linear", samples(curve({ FK(0.25, -3.7, L), FK(1.75, 12.3, C), FK(2.5, 4.4, L), FK(4, 1e6, L), FK(5, 1e6 + 1, L) }), 0, 5.5, 44))
	P("linear-24", samples(curve({ FK(0, 0.1, L), FK(1, 0.7, L) }), 0, 1, 24))
	P("linear-from-negative", samples(curve({ FK(-1, -5, L), FK(0.5, 5, L) }), -1, 0.5, 12))
end

-- Cubic: the tangents set, and what stands in for one that is not
do
	for _, case in { { nil, nil }, { 2, nil }, { nil, -3 }, { 2, -3 }, { 0, 0 } } do
		P("cubic", case[1], case[2], samples(curve({ FK(0, 1, K, nil, case[1]), FK(2, 4, K, case[2], nil) }), 0, 2, 8))
	end
	P("three", samples(curve({ FK(0, 0, K), FK(1, 3, K), FK(3, 4, K) }), 0, 3, 12))
	P("five", samples(curve({ FK(0, 0, K), FK(1, 3, K), FK(3, 4, K), FK(4, -2, K), FK(6, 0, K) }), 0, 6, 24))
	P("linear-cubic-linear", samples(curve({ FK(0, 0, L), FK(1, 3, K), FK(3, 4, L), FK(4, -2, L) }), 0, 4, 16))
	P("constant-cubic-constant", samples(curve({ FK(0, 0, C), FK(1, 3, K), FK(3, 4, C), FK(4, -2, L) }), 0, 4, 16))
	P("cubic-linear-cubic", samples(curve({ FK(0, 0, K), FK(1, 3, L), FK(3, 4, K), FK(4, -2, L) }), 0, 4, 16))
	P("left-set-after-linear", samples(curve({ FK(0, 0, L), FK(1, 3, K, 50, nil), FK(3, 4, K) }), 0, 3, 12))
	P("left-set-after-constant", samples(curve({ FK(0, 0, C), FK(1, 3, K, 50, nil), FK(3, 4, K) }), 1, 3, 4))
	P("middle-left-only", samples(curve({ FK(0, 0, K), FK(1, 3, K, 5, nil), FK(3, 4, K) }), 0, 3, 12))
	P("middle-right-only", samples(curve({ FK(0, 0, K), FK(1, 3, K, nil, 5), FK(3, 4, K) }), 0, 3, 12))
	P("middle-both", samples(curve({ FK(0, 0, K), FK(1, 3, K, 5, -1), FK(3, 4, K) }), 0, 3, 12))
	P("first-left-set", samples(curve({ FK(0, 0, K, 9, nil), FK(1, 3, K) }), 0, 1, 4))
	P("last-right-set", samples(curve({ FK(0, 0, K), FK(1, 3, K, nil, 9) }), 0, 1, 4))
	P("set-neighbours", samples(curve({ FK(0, 0, K, nil, 4), FK(1, 3, K), FK(3, 4, K, -7, nil) }), 0, 3, 6))
	P("flat-rise-flat", samples(curve({ FK(0, 2, K), FK(1, 2, K), FK(2, 5, K), FK(3, 5, K) }), 0, 3, 12))
	local f = curve({ FK(0, 0, K), FK(1, 3, K, 5, -1), FK(3, 4, K) })
	local key = f:GetKeyAtIndex(2) key.Interpolation = L f:InsertKey(key)
	P("tangents-kept", f:GetKeys())
	-- a middle key without tangents between two Cubic segments: slopes (a, b) over (1, 2) seconds
	for _, pair in { { 3, 0.5 }, { 0.1, 10 }, { -2, -4 }, { 0, 5 }, { 5, 0 }, { 2, 2 }, { 1, -1 }, { -3, 0.25 }, { 0.001, 7 } } do
		P("unset", pair[1], pair[2], samples(curve({ FK(0, 0, K), FK(1, pair[1], K), FK(3, pair[1] + 2 * pair[2], K) }), 0, 3, 6))
	end
	local row = {}
	for _, case in { { 7, 1, 2 }, { 3, 11, 0.5 }, { 0.37, 2.9, 1.7 }, { 5, 0.01, 3 }, { 123.4, 0.77, 0.9 }, { 1, 1, 1 }, { 2, 6, 1 }, { 6, 2, 1 }, { 2, 6.1, 1 }, { 2, 5.9, 1 }, { -0.3, -9, 2.2 } } do
		table.insert(row, S(curve({ FK(0, 0, K), FK(1, case[1], K), FK(1 + case[3], case[1] + case[2], K) }):GetValueAtTime(0.5)))
	end
	P("unset-half", table.concat(row, " "))
	f = curve({ FK(0.1, 0.3, K, nil, 1.7), FK(0.9, -2.2, K, 0.37, -4.1), FK(2.3, 5.9, L, 2.9, nil), FK(3.1, 0.123, L) })
	P("awkward", f:GetKeys())
	P("awkward", samples(f, 0.1, 3.1, 60))
	row = {}
	for _, t in { 0.1234, 0.5, 0.777, 0.9001, 1.0101, 1.6, 2.2999, 2.31, 2.7, 3.0999 } do table.insert(row, S(f:GetValueAtTime(t))) end
	P("awkward", table.concat(row, " "))
end
return table.concat(out, "\n")
