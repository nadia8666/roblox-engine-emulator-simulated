-- MarkerCurve:InsertMarkerAtTime: what a value that is no string becomes, and how long it may be. Runs unchanged in
-- Studio (the command bar or a plugin) and here; markers.txt is what Studio 0.747 returned (2026-11-11).
local out = {}
local function S(v) if type(v) == "number" then return string.format("%.9g", v) elseif type(v) == "table" then local t = {} for i, x in v do t[#t+1] = S(x) end local n = 0 for _ in v do n += 1 end return "{" .. table.concat(t, ", ") .. "}" .. (n ~= #v and "#" .. n or "") else return tostring(v) end end
local function P(...) local t = table.pack(...) for i = 1, t.n do t[i] = S(t[i]) end table.insert(out, table.concat(t, " ")) end
local function try(f, ...) local r = table.pack(pcall(f, ...)) if r[1] then local t = {} for i = 2, r.n do t[#t+1] = S(r[i]) end return "ok(" .. table.concat(t, ", ") .. ")" else return "E(" .. tostring(r[2]) .. ")" end end
local L, C, K = Enum.KeyInterpolationMode.Linear, Enum.KeyInterpolationMode.Constant, Enum.KeyInterpolationMode.Cubic
local function FK(t, v, m, lt, rt) local k = FloatCurveKey.new(t, v, m) if lt then k.LeftTangent = lt end if rt then k.RightTangent = rt end return k end
local function curve(keys) local f = Instance.new("FloatCurve") f:SetKeys(keys) return f end
do
	local m = Instance.new("MarkerCurve")
	local values = { 0.1, 1/3, 1e20, 123456789.125, 1/0, -1/0, 2^53, 1e-7, 100, -5.5, CFrame.new(1, 2, 3), Color3.new(1, 0.5, 0), UDim2.new(1, 2, 3, 4), Vector2.new(1.5, 2), Enum.Material, Enum.KeyInterpolationMode.Cubic, {1, 2}, {a = 1}, buffer.create(2), coroutine.create(print), newproxy(), "", Instance.new("Part"), BrickColor.new("Bright red"), TweenInfo.new(), NumberRange.new(1, 2), UDim.new(1, 2), Rect.new(1, 2, 3, 4), NumberSequence.new(1), Vector3int16.new(1, 2, 3), DateTime.fromUnixTimestamp(0), FloatCurveKey.new(1, 2, L), Font.fromEnum(Enum.Font.Arial), Ray.new(Vector3.zero, Vector3.one), Random.new(1), Faces.new(Enum.NormalId.Top), Region3.new(Vector3.zero, Vector3.one) }
	local row = {}
	for i, v in values do
		local r = try(m.InsertMarkerAtTime, m, i, v)
		local got = "?"
		for _, k in m:GetMarkers() do if k.Time == i then got = "[" .. string.gsub(tostring(k.Value), "%z", "\0") .. "]#" .. #k.Value end end
		table.insert(row, typeof(v) .. "=" .. got .. (if string.sub(r, 1, 2) == "ok" then "" else r))
	end
	P("marker-values", table.concat(row, " | "))
end
do
	local m = Instance.new("MarkerCurve")
	local row = {}
	for i, n in {62, 63, 64, 65, 100} do table.insert(row, n .. "=" .. try(m.InsertMarkerAtTime, m, i, string.rep("a", n))) end
	P("marker-long", table.concat(row, " "), m.Length)
	P("marker-same-time", try(m.InsertMarkerAtTime, m, 1, "again"), m.Length, m:GetMarkerAtIndex(1).Value)
	local v = Instance.new("Vector3Curve")
	P("xyz-args", try(v.X, v, 5), #v:GetChildren())
end
return table.concat(out, "\n")
