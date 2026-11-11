-- Vector3Curve and EulerRotationCurve: which FloatCurve is the X, Y or Z channel as children come, go and are
-- renamed. Runs unchanged in Studio (the command bar or a plugin) and here; channels.txt is what Studio 0.747 returned
-- (2026-11-11).
local out = {}
local function S(v) if type(v) == "number" then return string.format("%.9g", v) elseif type(v) == "table" then local t = {} for i, x in v do t[#t+1] = S(x) end local n = 0 for _ in v do n += 1 end return "{" .. table.concat(t, ", ") .. "}" .. (n ~= #v and "#" .. n or "") else return tostring(v) end end
local function P(...) local t = table.pack(...) for i = 1, t.n do t[i] = S(t[i]) end table.insert(out, table.concat(t, " ")) end
local function try(f, ...) local r = table.pack(pcall(f, ...)) if r[1] then local t = {} for i = 2, r.n do t[#t+1] = S(r[i]) end return "ok(" .. table.concat(t, ", ") .. ")" else return "E(" .. tostring(r[2]) .. ")" end end
local L, C, K = Enum.KeyInterpolationMode.Linear, Enum.KeyInterpolationMode.Constant, Enum.KeyInterpolationMode.Cubic
local function FK(t, v, m, lt, rt) local k = FloatCurveKey.new(t, v, m) if lt then k.LeftTangent = lt end if rt then k.RightTangent = rt end return k end
local function curve(keys) local f = Instance.new("FloatCurve") f:SetKeys(keys) return f end
local function fc(id, name) local f = Instance.new("FloatCurve") f.Name = name f:InsertKey(FloatCurveKey.new(0, id, L)) return f end
local function kids(v) local t = {} for _, c in v:GetChildren() do t[#t+1] = c.Name .. "=" .. (if c:IsA("FloatCurve") then (if c.Length > 0 then S(c:GetKeyAtIndex(1).Value) else "new") else c.ClassName) end return "[" .. table.concat(t, ",") .. "]" end
local function at(v) if v:IsA("Vector3Curve") then return try(v.GetValueAtTime, v, 0) else return try(v.GetAnglesAtTime, v, 0) end end
local function slot(v, which) local ok, c = pcall(v[which], v) if not ok then return "E(" .. tostring(c) .. ")" end if c == nil then return "nil" end return c.Name .. "=" .. (if c:IsA("FloatCurve") then (if c.Length > 0 then S(c:GetKeyAtIndex(1).Value) else "new") else c.ClassName) end
for _, class in { "Vector3Curve", "EulerRotationCurve" } do
	P("==", class)
	local function new() return Instance.new(class) end
	do local v = new() P("empty", at(v), kids(v)) P("X()", slot(v, "X"), kids(v), at(v)) P("X() again", slot(v, "X"), kids(v)) P("Z()", slot(v, "Z"), kids(v), at(v)) P("Y()", slot(v, "Y"), kids(v), at(v)) end
	do local v = new() local a = fc(1, "X") a.Parent = v P("A named", at(v), kids(v)) a.Name = "Q" P("A renamed", at(v), kids(v), slot(v, "X"), kids(v)) a.Name = "Y" P("A renamed Y", at(v), slot(v, "Y"), kids(v), at(v)) end
	do local v = new() local a = fc(1, "Q") a.Parent = v P("B parented Q", at(v), kids(v)) a.Name = "X" P("B renamed X", at(v), kids(v)) P("B X()", slot(v, "X"), kids(v), at(v)) end
	do local v = new() local a, b = fc(1, "X"), fc(2, "X") a.Parent = v b.Parent = v P("C two", at(v), slot(v, "X"), kids(v)) a.Parent = nil P("C first removed", at(v), kids(v)) P("C X()", slot(v, "X"), kids(v), at(v)) end
	do local v = new() local a, b = fc(1, "X"), fc(2, "X") a.Parent = v b.Parent = v b.Parent = nil P("C2 second removed", at(v), slot(v, "X"), kids(v)) end
	do local v = new() local a = fc(1, "X") a.Parent = v a.Name = "Q" a.Parent = nil P("D renamed holder removed", at(v), kids(v), slot(v, "X"), kids(v)) end
	do local v = new() local a = fc(1, "X") a.Parent = v a.Name = "Q" local b = fc(2, "X") b.Parent = v P("E renamed, new X added", at(v), slot(v, "X"), kids(v)) b.Parent = nil P("E new X removed", at(v), slot(v, "X"), kids(v)) end
	do local v = new() local a = fc(1, "X") a.Parent = v local b = fc(2, "Q") b.Parent = v b.Name = "X" b.Parent = nil P("F late-named X removed", at(v), slot(v, "X"), kids(v)) end
	do local v = new() local a = fc(1, "X") a.Parent = v local g = Instance.new("Folder") g.Name = "X" g.Parent = v P("G folder X added", at(v), kids(v)) P("G X()", slot(v, "X"), kids(v)) end
	do local v = new() local g = Instance.new("Folder") g.Name = "X" g.Parent = v local a = fc(1, "X") a.Parent = v P("G2 folder then curve", at(v), slot(v, "X"), kids(v)) g.Parent = nil P("G2 folder removed", at(v), slot(v, "X"), kids(v)) end
	do local v = new() local a = fc(1, "X") a.Parent = v local g = Instance.new("Folder") g.Name = "Q" g.Parent = v g.Name = "X" g.Parent = nil P("G3 late-named folder removed", at(v), kids(v)) end
	do local v, w = new(), new() local a = fc(1, "X") a.Parent = v a.Parent = w P("H moved", at(v), at(w), kids(v), kids(w)) a.Name = "Y" a.Parent = v P("H moved back as Y", at(v), at(w), kids(v), kids(w)) end
	do local v = new() local a = fc(1, "X") a.Parent = v a.Name = "Q" local c = v:Clone() P("I clone of renamed", at(c), kids(c), slot(c, "X"), kids(c)) end
	do local v = new() local a = fc(1, "Q") a.Parent = v a.Name = "X" local c = v:Clone() P("I2 clone of late-named", at(c), kids(c)) end
	do local v = new() local a, b, c = fc(1, "X"), fc(2, "Y"), fc(3, "Z") a.Parent = v b.Parent = v c.Parent = v a.Name, b.Name = "Y", "X" P("J swapped names", at(v), slot(v, "X"), slot(v, "Y"), kids(v)) v:ClearAllChildren() P("J cleared", at(v), kids(v)) end
	do local v = new() local a = fc(1, "X") a.Parent = v a.Name = "Q" a.Parent = nil a.Parent = v P("K renamed, out and in", at(v), kids(v)) end
	do local v = new() local a = fc(1, "x") a.Parent = v P("L lowercase", at(v), kids(v)) end
	do local v = new() local a = fc(1, "X") a.Parent = v a:Destroy() P("M destroyed", at(v), kids(v)) end
	do local v = new() local a = fc(1, "X") a.Parent = v local b = fc(2, "X") b.Parent = v a.Name = "Q" b.Parent = nil P("N holder renamed, twin removed", at(v), slot(v, "X"), kids(v)) end
end
-- channel slots, more
do
	local function fc2(id, name) local f = Instance.new("FloatCurve") f.Name = name f:InsertKey(FloatCurveKey.new(0, id, L)) return f end
	local v = Instance.new("Vector3Curve") local a = fc2(1, "X") a.Parent = v a.Name = "Q" a:Destroy()
	P("slot destroyed holder", try(v.GetValueAtTime, v, 0), try(function() local x = v:X() return x.Name, x.Parent == nil, x.Length end), #v:GetChildren())
	local b = fc2(2, "X") b.Parent = v P("slot then new X", try(v.GetValueAtTime, v, 0), #v:GetChildren())
	local w = Instance.new("Vector3Curve") local g = Instance.new("Folder") g.Name = "X" g.Parent = w
	P("slot folder only", try(w.GetValueAtTime, w, 0), try(function() return w:X().ClassName end), #w:GetChildren())
	local u = Instance.new("Vector3Curve") local d = fc2(3, "X") d.Parent = u local h = Instance.new("Folder") h.Name = "X" h.Parent = u h.Parent = nil
	P("slot folder X removed", try(u.GetValueAtTime, u, 0))
	local n = Instance.new("Vector3Curve") local e = Instance.new("FloatCurve", n) e.Name = "X" e:InsertKey(FloatCurveKey.new(0, 4, L))
	P("slot made with parent", try(n.GetValueAtTime, n, 0))
	local nv = Instance.new("NumberValue") nv.Name = "Y" nv.Parent = n P("slot number value", try(n.GetValueAtTime, n, 0), try(function() return n:Y().ClassName end), #n:GetChildren())
end
return table.concat(out, "\n")
