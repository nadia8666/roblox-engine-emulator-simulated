-- FloatCurveKey and RotationCurveKey: construction, members, errors and tostring. Runs unchanged in Studio (the
-- command bar or a plugin) and here; keys.txt is what Studio 0.747 returned (2026-11-11).
local out = {}
local function S(v) if type(v) == "number" then return string.format("%.9g", v) elseif type(v) == "table" then local t = {} for i, x in v do t[#t+1] = S(x) end local n = 0 for _ in v do n += 1 end return "{" .. table.concat(t, ", ") .. "}" .. (n ~= #v and "#" .. n or "") else return tostring(v) end end
local function P(...) local t = table.pack(...) for i = 1, t.n do t[i] = S(t[i]) end table.insert(out, table.concat(t, " ")) end
local function try(f, ...) local r = table.pack(pcall(f, ...)) if r[1] then local t = {} for i = 2, r.n do t[#t+1] = S(r[i]) end return "ok(" .. table.concat(t, ", ") .. ")" else return "E(" .. tostring(r[2]) .. ")" end end
local L, C, K = Enum.KeyInterpolationMode.Linear, Enum.KeyInterpolationMode.Constant, Enum.KeyInterpolationMode.Cubic

P("fck", try(FloatCurveKey.new, 0, 1, L), try(FloatCurveKey.new, 0.5, 2, C), try(FloatCurveKey.new, 1.25, -3, K))
P("fck-args", try(FloatCurveKey.new), try(FloatCurveKey.new, 1), try(FloatCurveKey.new, 1, 2), try(FloatCurveKey.new, "a", 2, L), try(FloatCurveKey.new, 1, 2, 3), try(FloatCurveKey.new, 1, 2, "Linear"), try(FloatCurveKey.new, 1, 2, Enum.Material.Plastic), try(FloatCurveKey.new, 1, 2, L, 4))
P("fck-args-more", try(FloatCurveKey.new, 1, 2, 1), try(FloatCurveKey.new, 1, 2, nil), try(FloatCurveKey.new, nil, nil, L), try(FloatCurveKey.new, "2", "3", L), try(FloatCurveKey.new, true, {}, L))
local k = FloatCurveKey.new(1.5, 2.5, K)
P("fck-props", k.Time, k.Value, k.Interpolation, k.LeftTangent, k.RightTangent, typeof(k), type(k), getmetatable(k))
P("fck-bad", try(function() return k.Nope end), try(function() k.Nope = 1 end))
P("fck-set", try(function() k.Time = 3 return k.Time end), try(function() k.Value = 4 return k.Value end), try(function() k.Interpolation = L return k.Interpolation end), try(function() k.LeftTangent = 0.5 return k.LeftTangent end), try(function() k.RightTangent = -2 return k.RightTangent end), tostring(k))
P("fck-set-nil", try(function() k.LeftTangent = nil return k.LeftTangent end), tostring(k))
P("fck-set-bad", try(function() k.Time = "x" return k.Time end), try(function() k.Interpolation = 1 end), try(function() k.Interpolation = "Cubic" return k.Interpolation end), try(function() k.Interpolation = Enum.Material.Plastic end), try(function() k.Time = -1 return k.Time end), try(function() k.Value = "7" return k.Value end), try(function() k.Value = nil return k.Value end))
P("fck-eq", FloatCurveKey.new(0, 1, L) == FloatCurveKey.new(0, 1, L), FloatCurveKey.new(0, 1, L) == FloatCurveKey.new(0, 2, L), k == k)
P("fck-text", tostring(FloatCurveKey.new(0 / 0, 1 / 0, L)), tostring(FloatCurveKey.new(-1 / 0, -0, L)), tostring(FloatCurveKey.new(1e20, -1e-20, L)), tostring(FloatCurveKey.new(0.1, 1 / 3, L)), tostring(FloatCurveKey.new(1e-5, 123456789, L)), tostring(FloatCurveKey.new(0.5, 1e9, L)), tostring(FloatCurveKey.new(100000, 1234567.5, L)))
P("fck-lib", try(function() return FloatCurveKey.Nope end), typeof(FloatCurveKey))
local t = FloatCurveKey.new(0.1, 0.2, K)
t.RightTangent = 1.1 P("fck-rt", tostring(t), t.RightTangent)
t.Interpolation = L P("fck-rt-linear", tostring(t), t.RightTangent)
t.Interpolation = K P("fck-rt-back", tostring(t), t.RightTangent)
t.RightTangent = 2 P("fck-rt-nil", try(function() t.RightTangent = nil return t.RightTangent end), tostring(t))
local l = FloatCurveKey.new(0, 1, L) P("fck-linear-rt-nil", try(function() l.RightTangent = nil return l.RightTangent end))

local r = RotationCurveKey.new(0.5, CFrame.new(1, 2, 3) * CFrame.Angles(0.5, 0, 0), K)
P("rck", tostring(r), typeof(r), getmetatable(r))
P("rck-props", r.Time, r.Value, r.Interpolation, r.LeftTangent, r.RightTangent)
P("rck-args", try(RotationCurveKey.new), try(RotationCurveKey.new, 1), try(RotationCurveKey.new, 1, CFrame.new()), try(RotationCurveKey.new, 1, 2, L), try(RotationCurveKey.new, 1, CFrame.new(), 3), try(RotationCurveKey.new, 1, Vector3.new(), L))
P("rck-set", try(function() r.Time = 3 return r.Time end), try(function() r.Value = CFrame.Angles(1, 0, 0) return r.Value end), try(function() r.Interpolation = L return r.Interpolation end), try(function() r.LeftTangent = 0.5 return r.LeftTangent end), try(function() r.RightTangent = -2 return r.RightTangent end), tostring(r))
P("rck-bad", try(function() return r.Nope end), try(function() r.Nope = 1 end), try(function() r.LeftTangent = nil return r.LeftTangent end), try(function() r.Value = 5 return r.Value end), try(function() r.Time = nil return r.Time end), RotationCurveKey.new(0, CFrame.new(), L) == RotationCurveKey.new(0, CFrame.new(), L))
local q = RotationCurveKey.new(0, CFrame.new(), K)
P("rck-default", tostring(q), q.LeftTangent, q.RightTangent)
q.RightTangent = 2 P("rck-rt", tostring(q))
q.LeftTangent = 3 P("rck-lt", tostring(q))
q.LeftTangent = nil P("rck-lt-nil", tostring(q), q.LeftTangent, q.RightTangent)
local n = RotationCurveKey.new(0, CFrame.new(), L) P("rck-linear", tostring(n), try(function() n.RightTangent = 1 end), try(function() n.RightTangent = nil return n.RightTangent end), tostring(n))
P("rck-not-orthonormal", tostring(RotationCurveKey.new(0, CFrame.new(0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2), L)))
return table.concat(out, "\n")
