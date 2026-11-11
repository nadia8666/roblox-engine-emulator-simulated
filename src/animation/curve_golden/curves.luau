-- RotationCurve, EulerRotationCurve, Vector3Curve, MarkerCurve and CurveAnimation's own members. Runs unchanged in
-- Studio (the command bar or a plugin) and here; curves.txt is what Studio 0.747 returned (2026-11-11). Rotations
-- print as an angle about X with a `~` before it: Studio's own float32 arithmetic leaves the seventh digit open, and the
-- test compares those numbers within 2e-6.
local out = {}
local function S(v) if type(v) == "number" then return string.format("%.9g", v) elseif type(v) == "table" then local t = {} for i, x in v do t[#t+1] = S(x) end local n = 0 for _ in v do n += 1 end return "{" .. table.concat(t, ", ") .. "}" .. (n ~= #v and "#" .. n or "") else return tostring(v) end end
local function P(...) local t = table.pack(...) for i = 1, t.n do t[i] = S(t[i]) end table.insert(out, table.concat(t, " ")) end
local function try(f, ...) local r = table.pack(pcall(f, ...)) if r[1] then local t = {} for i = 2, r.n do t[#t+1] = S(r[i]) end return "ok(" .. table.concat(t, ", ") .. ")" else return "E(" .. tostring(r[2]) .. ")" end end
local L, C, K = Enum.KeyInterpolationMode.Linear, Enum.KeyInterpolationMode.Constant, Enum.KeyInterpolationMode.Cubic
local function FK(t, v, m) return FloatCurveKey.new(t, v, m) end
-- (a tangent given as false is unset)
local function RK(t, cf, m, lt, rt) local k = RotationCurveKey.new(t, cf, m) if lt ~= nil then k.LeftTangent = lt or nil end if rt ~= nil then k.RightTangent = rt or nil end return k end
local function rcurve(keys) local r = Instance.new("RotationCurve") for _, k in keys do r:InsertKey(k) end return r end
local function CF(cf) if typeof(cf) ~= "CFrame" then return tostring(cf) end local t = { cf:GetComponents() } for i = 1, 12 do t[i] = S(t[i]) end return "[" .. table.concat(t, ",") .. "]" end
local function angle(cf) local _, _, _, _, _, _, _, m11, _, _, m21 = cf:GetComponents() return "~" .. S(math.atan2(m21, m11)) end
local function turns(r, from, to, n) local row = {} for i = 0, n do table.insert(row, angle(r:GetValueAtTime(from + (to - from) * i / n))) end return table.concat(row, " ") end
local function triple(t) local parts = {} for i = 1, 3 do parts[i] = S(t[i]) end local n = 0 for _ in t do n += 1 end return "{" .. table.concat(parts, ", ") .. "}#" .. n end
local RX = function(a) return CFrame.Angles(a, 0, 0) end

-- RotationCurve
do
	local r = Instance.new("RotationCurve")
	P("r-empty", r.Length, try(r.GetValueAtTime, r, 0), try(r.GetKeys, r), try(r.GetKeyAtIndex, r, 1), try(r.GetKeyIndicesAtTime, r, 0), try(r.RemoveKeyAtIndex, r, 1, 1), try(r.RemoveKeyAtIndex, r, 0, 1), try(function() r.Length = 1 end))
	P("r-insert", try(r.InsertKey, r, RK(1, RX(1), L)), try(r.InsertKey, r, RK(0, RX(0), L)), try(r.InsertKey, r, RK(1, RX(0), C)), try(r.InsertKey, r, FloatCurveKey.new(0, 1, L)), try(r.InsertKey, r, 5), try(r.InsertKey, r, Vector3.zero), try(r.InsertKey, r), r.Length)
	P("r-keys", r:GetKeys())
	P("r-index", try(r.GetKeyAtIndex, r, 0), try(r.GetKeyAtIndex, r, 3), r:GetKeyIndicesAtTime(-1), r:GetKeyIndicesAtTime(0), r:GetKeyIndicesAtTime(0.5), r:GetKeyIndicesAtTime(1), r:GetKeyIndicesAtTime(2), try(r.GetValueAtTime, r, "x"), try(r.GetValueAtTime, r))
	P("r-set", try(r.SetKeys, r, { RotationCurveKey.new(0, CFrame.new(), L) }), try(r.SetKeys, r, r:GetKeys()), try(r.SetKeys, r, { FloatCurveKey.new(0, 1, L) }), try(r.SetKeys, r, 3), try(r.SetKeys, r), r.Length, try(r.SetKeys, r, {}), r.Length)
	r:InsertKey(RK(0, RX(0), L)) r:InsertKey(RK(1, RX(1), L)) r:InsertKey(RK(2, RX(1), L))
	P("r-remove", try(r.RemoveKeyAtIndex, r, 2), r.Length, try(r.RemoveKeyAtIndex, r, 1, 5), r.Length, try(r.RemoveKeyAtIndex, r, 1, -1))
	local one = rcurve({ RK(1, RX(0.5), K) })
	P("r-one", angle(one:GetValueAtTime(0)), angle(one:GetValueAtTime(5)), one:GetValueAtTime(0).Position)
	P("r-kept", rcurve({ RK(0, RX(0), K, false, false), RK(2, RX(0), K, 1.5, false), RK(3, RX(0), L, false) }):GetKeys())
end
do
	P("linear", turns(rcurve({ RK(0, RX(0), L), RK(2, RX(1), L) }), -0.5, 2.5, 12))
	P("constant", turns(rcurve({ RK(0, RX(0), C), RK(2, RX(1), L) }), 0, 2, 8))
	P("cubic", turns(rcurve({ RK(0, RX(0), K), RK(2, RX(1), K) }), 0, 2, 8))
	P("cubic-unset", turns(rcurve({ RK(0, RX(0), K, false, false), RK(2, RX(1), K, false, false) }), 0, 2, 8))
	P("cubic-right-1", turns(rcurve({ RK(0, RX(0), K, nil, 1), RK(2, RX(1), K) }), 0, 2, 8))
	P("cubic-right-1-one-second", turns(rcurve({ RK(0, RX(0), K, nil, 1), RK(1, RX(1), K) }), 0, 1, 8))
	P("cubic-right-1-two-radians", turns(rcurve({ RK(0, RX(0), K, nil, 1), RK(2, RX(2), K) }), 0, 2, 8))
	P("cubic-left-minus-2", turns(rcurve({ RK(0, RX(0), K), RK(2, RX(1), K, -2, nil) }), 0, 2, 8))
	P("cubic-both", turns(rcurve({ RK(0, RX(0), K, nil, 0.7), RK(2, RX(1), K, 1.3, nil) }), 0, 2, 8))
	P("cubic-past-the-ends", turns(rcurve({ RK(0, RX(0), K, nil, 5), RK(2, RX(1), K, 5, nil) }), 0, 2, 8))
	P("three", turns(rcurve({ RK(0, RX(0), K), RK(1, RX(1), K), RK(3, RX(1.5), K) }), 0, 3, 12))
	P("three-unset", turns(rcurve({ RK(0, RX(0), K, false, false), RK(1, RX(1), K, false, false), RK(3, RX(1.5), K, false, false) }), 0, 3, 12))
	P("linear-cubic-linear", turns(rcurve({ RK(0, RX(0), L), RK(1, RX(1), K, false, false), RK(3, RX(1.5), L), RK(4, RX(0), L) }), 0, 4, 16))
	P("the-short-way", turns(rcurve({ RK(0, RX(0), L), RK(2, RX(3.5), L) }), 0, 2, 8))
	P("near-key", turns(rcurve({ RK(0, RX(0), L), RK(1, RX(1), L), RK(2, RX(3), L) }), 0.9998, 1.0002, 4))
end

-- EulerRotationCurve
do
	local e = Instance.new("EulerRotationCurve")
	P("e-empty", e.RotationOrder, #e:GetChildren(), triple(e:GetAnglesAtTime(0)), CF(e:GetRotationAtTime(0)), #e:GetChildren())
	local x = e:X()
	P("e-x", x.ClassName, x.Name, x.Parent == e, #e:GetChildren(), e:X() == x, x.Length)
	e:Y() e:Z() P("e-made", #e:GetChildren(), triple(e:GetAnglesAtTime(0)))
	x:InsertKey(FK(0, 0.3, L)) P("e-x-key", triple(e:GetAnglesAtTime(0)), CF(e:GetRotationAtTime(0)))
	P("e-bad", try(e.GetAnglesAtTime, e), try(e.GetRotationAtTime, e), try(e.GetAnglesAtTime, e, "x"), try(e.GetRotationAtTime, e, {}), CF(e:GetRotationAtTime("0.5")))
end
do
	local e = Instance.new("EulerRotationCurve")
	local function channel(name, v) local f = Instance.new("FloatCurve") f.Name = name f:SetKeys({ FK(0, v, L), FK(1, v * 2, L) }) f.Parent = e return f end
	local y = channel("Y", 0.5)
	P("e-y-only", e:Y() == y, triple(e:GetAnglesAtTime(0)), #e:GetChildren(), CF(e:GetRotationAtTime(0)))
	local x = channel("X", 0.3) channel("Z", 0.7)
	P("e-angles", triple(e:GetAnglesAtTime(0)), triple(e:GetAnglesAtTime(0.5)), triple(e:GetAnglesAtTime(2)), #e:GetChildren())
	for _, order in Enum.RotationOrder:GetEnumItems() do
		e.RotationOrder = order
		P("e-order", order.Name, CF(e:GetRotationAtTime(0)), CF(e:GetRotationAtTime(0.5)))
	end
	local second = channel("X", 9) P("e-two-of-a-name", e:X() == x, e:X() == second)
	local other = Instance.new("EulerRotationCurve")
	local value = Instance.new("NumberValue") value.Name = "X" value.Parent = other
	local lower = Instance.new("FloatCurve") lower.Name = "x" lower.Parent = other
	local made = other:X()
	P("e-other-children", made.ClassName, made ~= lower, #other:GetChildren(), triple(other:GetAnglesAtTime(0)))
end

-- Vector3Curve
do
	local v = Instance.new("Vector3Curve")
	P("v-empty", #v:GetChildren(), triple(v:GetValueAtTime(0)), #v:GetChildren())
	local function channel(name, a) local f = Instance.new("FloatCurve") f.Name = name f:SetKeys({ FK(0, a, L), FK(1, a * 2, L) }) f.Parent = v return f end
	local y = channel("Y", 0.5)
	P("v-y-only", v:Y() == y, triple(v:GetValueAtTime(0.5)), #v:GetChildren())
	channel("X", 1) channel("Z", 4)
	P("v-all", triple(v:GetValueAtTime(0.5)), triple(v:GetValueAtTime(-1)), triple(v:GetValueAtTime(9)), triple(v:GetValueAtTime("0.25")), try(v.GetValueAtTime, v), try(v.GetValueAtTime, v, "x"))
	local w = Instance.new("Vector3Curve") local made = w:Z()
	P("v-made", made.Name, made.ClassName, made.Parent == w, #w:GetChildren(), triple(w:GetValueAtTime(0)))
end

-- MarkerCurve
do
	local m = Instance.new("MarkerCurve")
	P("m-empty", m.Length, try(m.GetMarkers, m), try(m.GetMarkerAtIndex, m, 1), try(m.GetMarkerAtIndex, m, 0), try(m.GetMarkerAtIndex, m), try(m.RemoveMarkerAtIndex, m, 1, 1), try(m.RemoveMarkerAtIndex, m, 0, 1), try(m.RemoveMarkerAtIndex, m, 1), try(function() m.Length = 1 end))
	P("m-insert", try(m.InsertMarkerAtTime, m, 1, "one"), try(m.InsertMarkerAtTime, m, 0.5, "half"), try(m.InsertMarkerAtTime, m, 1, "again"), try(m.InsertMarkerAtTime, m, 2), try(m.InsertMarkerAtTime, m), try(m.InsertMarkerAtTime, m, "3", 7), try(m.InsertMarkerAtTime, m, 1.0003, "near"), try(m.InsertMarkerAtTime, m, -1, "before"), try(m.InsertMarkerAtTime, m, "x", "v"), try(m.InsertMarkerAtTime, m, {}, "v"), m.Length)
	local list = {}
	for i, marker in m:GetMarkers() do local parts = {} for key, value in marker do table.insert(parts, tostring(key) .. "=" .. S(value) .. ":" .. typeof(value)) end table.sort(parts) table.insert(list, i .. "{" .. table.concat(parts, ",") .. "}") end
	P("m-markers", table.concat(list, " "))
	local one = m:GetMarkerAtIndex(2)
	P("m-at", one.Time, one.Value, try(m.GetMarkerAtIndex, m, 99), try(m.GetMarkerAtIndex, m, -1))
	P("m-remove", try(m.RemoveMarkerAtIndex, m, 2, 2), m.Length, try(m.RemoveMarkerAtIndex, m, 1, -1), try(m.RemoveMarkerAtIndex, m, 9, 1), try(m.RemoveMarkerAtIndex, m, 1, 99), m.Length)
end

-- CurveAnimation
do
	local c = Instance.new("CurveAnimation")
	P("c-new", c.Length, c.Loop, c.Priority, try(function() c.Length = 1 end))
	local function folder(name, parent) local f = Instance.new("Folder") f.Name = name f.Parent = parent return f end
	local function float(name, last, parent) local f = Instance.new("FloatCurve") f.Name = name f:SetKeys({ FK(0, 0, L), FK(last, 1, L) }) f.Parent = parent return f end
	local function length(build) local clip = Instance.new("CurveAnimation") build(clip) return clip.Length end
	P("c-length", length(function(clip) local p = Instance.new("Vector3Curve") p.Name = "Position" p.Parent = folder("Arm", folder("Root", clip)) float("X", 1.5, p) end),
		length(function(clip) float("Any", 3, folder("Arm", clip)) end),
		length(function(clip) float("Any", 3, folder("FaceControls", folder("Head", clip))) end),
		length(function(clip) local p = Instance.new("Vector3Curve") p.Name = "Position" p.Parent = clip float("X", 3, p) end),
		length(function(clip) float("Stray", 9, clip) end),
		length(function(clip) local q = Instance.new("RotationCurve") q.Name = "Other" q.Parent = folder("Arm", clip) q:InsertKey(RotationCurveKey.new(4, CFrame.new(), L)) end),
		length(function(clip) local m = Instance.new("MarkerCurve") m.Parent = folder("Arm", clip) m:InsertMarkerAtTime(3, "x") end),
		length(function(clip) local m = Instance.new("MarkerCurve") m.Parent = clip m:InsertMarkerAtTime(3, "x") end),
		length(function(clip) local p = Instance.new("Vector3Curve") p.Name = "Position" p.Parent = folder("Arm", clip) float("X", 1.2345, p) end),
		length(function(clip) local held = Instance.new("Model") held.Parent = clip float("Any", 3, folder("Arm", held)) end))
end
return table.concat(out, "\n")
