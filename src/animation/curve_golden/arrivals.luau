-- A marker value with a zero byte (the live curve keeps it; a clone, a track and the copies of a registered clip have
-- none of the curve's markers), and which FloatCurve is a channel of a Vector3Curve that was cloned whole. Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode;
-- arrivals.txt is what Studio 0.747 returned (2026-11-11).
local out = {}
local function S(v) if type(v) == "number" then return string.format("%.9g", v) elseif type(v) == "table" then local t = {} for i, x in v do t[#t+1] = S(x) end local n = 0 for _ in v do n += 1 end return "{" .. table.concat(t, ", ") .. "}" .. (n ~= #v and "#" .. n or "") else return tostring(v) end end
local function P(...) local t = table.pack(...) for i = 1, t.n do t[i] = S(t[i]) end table.insert(out, table.concat(t, " ")) end
local function try(f, ...) local r = table.pack(pcall(f, ...)) if r[1] then local t = {} for i = 2, r.n do t[#t+1] = S(r[i]) end return "ok(" .. table.concat(t, ", ") .. ")" else return "E(" .. tostring(r[2]) .. ")" end end
local L, C, K = Enum.KeyInterpolationMode.Linear, Enum.KeyInterpolationMode.Constant, Enum.KeyInterpolationMode.Cubic
local function FK(t, v, m, lt, rt) local k = FloatCurveKey.new(t, v, m) if lt then k.LeftTangent = lt end if rt then k.RightTangent = rt end return k end
local function curve(keys) local f = Instance.new("FloatCurve") f:SetKeys(keys) return f end
local ACP, KSP = game:GetService("AnimationClipProvider"), game:GetService("KeyframeSequenceProvider")
local function CF(cf) if typeof(cf) ~= "CFrame" then return tostring(cf) end local t = {cf:GetComponents()} for i = 1, 12 do t[i] = S(t[i]) end return "[" .. table.concat(t, ",") .. "]" end
local function fc(name, keys, parent) local f = Instance.new("FloatCurve") f.Name = name f:SetKeys(keys) f.Parent = parent return f end
local function folder(name, parent) local f = Instance.new("Folder") f.Name = name f.Parent = parent return f end
local function rig()
	local model = Instance.new("Model") model.Name = "CurveProbe_tmp"
	local root = Instance.new("Part") root.Name = "Root" root.Anchored = true root.Parent = model
	local arm = Instance.new("Part") arm.Name = "Arm" arm.Parent = model
	local hand = Instance.new("Part") hand.Name = "Hand" hand.Parent = model
	local j1 = Instance.new("Motor6D") j1.Name = "Shoulder" j1.Part0 = root j1.Part1 = arm j1.Parent = root
	local j2 = Instance.new("Motor6D") j2.Name = "Wrist" j2.Part0 = arm j2.Part1 = hand j2.Parent = arm
	local bone = Instance.new("Bone") bone.Name = "Tail" bone.Parent = hand
	model.PrimaryPart = root
	local controller = Instance.new("AnimationController") controller.Parent = model
	local animator = Instance.new("Animator") animator.Parent = controller
	return model, animator, j1, j2, bone
end
local function load(animator, clip)
	local id = ACP:RegisterAnimationClip(clip)
	local animation = Instance.new("Animation") animation.AnimationId = id
	local track = animator:LoadAnimation(animation)
	local waited = 0
	while track.Length == 0 and waited < 40 do task.wait() waited += 1 end
	return track, id
end
local function posX(parent, keys) local p = Instance.new("Vector3Curve") p.Name = "Position" p.Parent = parent fc("X", keys, p) return p end
local function eulerX(parent, keys) local r = Instance.new("EulerRotationCurve") r.Name = "Rotation" r.Parent = parent fc("X", keys, r) return r end
local function angX(cf) local _, _, _, _, _, _, _, m11, m12, _, m21, m22 = cf:GetComponents() return math.atan2(m21, m11) end
local function scene(label, body)
	local model, animator, j1, j2, bone = rig()
	local ok, problem = pcall(function() model.Parent = workspace body(animator, j1, j2, bone) end)
	model:Destroy()
	if not ok then P(label, "FAILED", problem) end
end
local function hex(s) return (s:gsub(".", function(c) return string.format("%02x", c:byte()) end)) end
local m = Instance.new("MarkerCurve") m.Name = "M"
m:InsertMarkerAtTime(0, "a\0b") m:InsertMarkerAtTime(0.5, "\0") m:InsertMarkerAtTime(1, "x\0\0y\0")
local function vals(c) local t = {} for _, k in c:GetMarkers() do t[#t + 1] = string.format("%g:%d:%s", k.Time, #k.Value, hex(k.Value)) end return table.concat(t, " ") end
P("live", vals(m))
P("clone", vals(m:Clone()))
scene("zero", function(animator, j1)
	local c = Instance.new("CurveAnimation") c.Loop = false
	posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(1, 10, L)})
	m:Clone().Parent = c
	local t, id = load(animator, c)
	local got = {}
	t:GetMarkerReachedSignal("M"):Connect(function(v) got[#got + 1] = #v .. ":" .. hex(v) end)
	t:Play(0) animator:StepAnimations(0.6) animator:StepAnimations(0.6) task.wait()
	P("signal", table.concat(got, " "))
	local ks = KSP:GetKeyframeSequenceAsync(id)
	local list = {}
	for _, d in ks:GetDescendants() do if d:IsA("KeyframeMarker") then list[#list + 1] = #d.Value .. ":" .. hex(d.Value) end end
	P("sequence", table.concat(list, " "))
	local copy = ACP:GetAnimationClipAsync(id)
	P("clip copy", vals(copy:FindFirstChild("M")))
end)
-- channels of curves that arrive whole
local function chan(c) local x = c:X() return x and (x.Name .. (x.Parent == c and "" or "(out)")) or "nil" end
local v = Instance.new("Vector3Curve") local fx = Instance.new("FloatCurve") fx.Name = "X" fx.Parent = v local fq = Instance.new("FloatCurve") fq.Name = "Q" fq.Parent = v
local c1 = v:Clone() c1:FindFirstChild("X").Name = "Was" P("clone renamed before use", chan(c1), #c1:GetChildren())
local c2 = v:Clone() c2:FindFirstChild("Q").Name = "X" c2:FindFirstChild("X").Name = "Gone" P("clone swap", chan(c2))
local c3 = v:Clone() c3:FindFirstChild("X"):Destroy() P("clone destroyed before use", chan(c3), #c3:GetChildren())
local holder = Instance.new("Folder") v:Clone().Parent = holder
local h2 = holder:Clone() local hv = h2:GetChildren()[1] hv:FindFirstChild("X").Name = "Was" P("deep clone renamed", chan(hv), v.Name)
local two = Instance.new("Vector3Curve") local a = Instance.new("FloatCurve") a.Name = "X" a:InsertKey(FloatCurveKey.new(0, 1, L)) a.Parent = two local b = Instance.new("FloatCurve") b.Name = "X" b:InsertKey(FloatCurveKey.new(0, 2, L)) b.Parent = two
a:Destroy()
local t2 = two:Clone() P("clone of emptied slot", chan(t2), t2:GetValueAtTime(0) and S(t2:GetValueAtTime(0)[1]) or "nil")
return table.concat(out, "\n")
