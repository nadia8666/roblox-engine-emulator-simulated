-- The translations a CurveAnimation's track gives a joint, to the last digit: a 10 s clip read at its 30 samples a
-- second, half way between them and at random times, at its start and far along it; times set against times
-- stepped; clips of other lengths; and times a hair from a sample. Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode; playback_poses.txt is what
-- Studio 0.747 returned (2026-11-11).
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
do
local fb = buffer.create(4)
local function f32(x) buffer.writef32(fb, 0, x) return buffer.readf32(fb, 0) end
scene("grid", function(animator, j1)
	local c = Instance.new("CurveAnimation") c.Loop = false
	local p = Instance.new("Vector3Curve") p.Name = "Position" p.Parent = folder("Arm", folder("Root", c))
	fc("X", {FK(0, 0, L), FK(10, 100, L)}, p)
	fc("Y", {FK(1, 10, L), FK(2, 20, L), FK(10, 37, L)}, p)
	local zig = {}
	for k = 0, 300 do table.insert(zig, FK((80 * k - 40) / 2400, -1000, L)) table.insert(zig, FK((80 * k + 39) / 2400, 975, L)) end
	fc("Z", zig, p)
	local t = load(animator, c)
	P("length", t.Length)
	t:Play(0, 1, 0)
	local function at(tp) t.TimePosition = tp animator:StepAnimations(0) local tr = j1.Transform return S(t.TimePosition) .. " " .. S(tr.X) .. " " .. S(tr.Y) .. " " .. S(tr.Z) end
	for k = 0, 75 do P("k", k, at(k / 30)) P("m", k, at((k + 0.5) / 30)) end
	local rng = Random.new(1)
	for i = 1, 60 do P("r", i, at(rng:NextNumber(0, 10))) end
end)
end
do
local fb = buffer.create(4)
local function f32(x) buffer.writef32(fb, 0, x) return buffer.readf32(fb, 0) end
scene("grid", function(animator, j1)
	local c = Instance.new("CurveAnimation") c.Loop = false
	local p = Instance.new("Vector3Curve") p.Name = "Position" p.Parent = folder("Arm", folder("Root", c))
	fc("X", {FK(0, 0, L), FK(10, 100, L)}, p)
	fc("Y", {FK(1, 10, L), FK(2, 20, L), FK(10, 37, L)}, p)
	local zig = {}
	for k = 0, 300 do table.insert(zig, FK((80 * k - 40) / 2400, -1000, L)) table.insert(zig, FK((80 * k + 39) / 2400, 975, L)) end
	fc("Z", zig, p)
	local t = load(animator, c)
	P("length", t.Length)
	t:Play(0, 1, 0)
	local function at(tp) t.TimePosition = tp animator:StepAnimations(0) local tr = j1.Transform return S(t.TimePosition) .. " " .. S(tr.X) .. " " .. S(tr.Y) .. " " .. S(tr.Z) end
	for k = 226, 300 do P("k", k, at(k / 30)) P("m", k, at((k + 0.5) / 30)) end
	local rng = Random.new(4)
	for i = 1, 60 do P("r", i, at(rng:NextNumber(0, 10))) end
end)
end
do
local function G(v) return string.format("%.17g", v) end
scene("step-vs-set", function(animator, j1)
	local c = Instance.new("CurveAnimation") c.Loop = true
	posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(1, 10, L)})
	local t = load(animator, c)
	t:Play(0, 1, 1) animator:StepAnimations(0)
	for i = 1, 8 do animator:StepAnimations(0.3) P("step", i, G(t.TimePosition), S(j1.Transform.X)) end
	t:AdjustSpeed(0)
	for _, tp in {0.3, 0.30000001192092896, 0.6, 0.60000002384185791, 0.9, 0.9000000357627869, 0.89999997615814209, 0.20000004768371582, 0.2, 0.5000000596046448, 0.80000007152557373} do
		t.TimePosition = tp animator:StepAnimations(0) P("set", G(tp), G(t.TimePosition), S(j1.Transform.X))
	end
	t:AdjustSpeed(1) t.TimePosition = 0 animator:StepAnimations(0)
	for i = 1, 6 do animator:StepAnimations(0.15) P("step.15", i, G(t.TimePosition), S(j1.Transform.X)) end
	t.TimePosition = 0 animator:StepAnimations(0)
	animator:StepAnimations(0.9) P("one step .9", G(t.TimePosition), S(j1.Transform.X))
	t.TimePosition = 0.6 animator:StepAnimations(0.3) P("set .6 step .3", G(t.TimePosition), S(j1.Transform.X))
	t.TimePosition = 0.60000002384185791 animator:StepAnimations(0.3) P("set .6f step .3", G(t.TimePosition), S(j1.Transform.X))
	t.TimePosition = 0.9000000357627869 animator:StepAnimations(0) P("set .9f step 0 at speed 1", G(t.TimePosition), S(j1.Transform.X))
end)
end
do
local function G(v) return string.format("%.17g", v) end
for _, case in {{1, true}, {1, false}, {2, false}, {0.5, false}, {3.7, false}, {10, false}, {10.0162497, false}} do
	scene("grid", function(animator, j1)
		local length, loop = case[1], case[2]
		local c = Instance.new("CurveAnimation") c.Loop = loop
		posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(length, 10 * length, L)})
		local t = load(animator, c)
		t:Play(0, 1, 0)
		local row = {}
		local n = math.min(40, math.floor(length * 30))
		for k = 0, n do t.TimePosition = k / 30 animator:StepAnimations(0) table.insert(row, S(j1.Transform.X)) t.TimePosition = (k + 0.5) / 30 animator:StepAnimations(0) table.insert(row, S(j1.Transform.X)) end
		P("grid", length, loop, G(t.Length), table.concat(row, " "))
	end)
end
end
do
local function G(v) return string.format("%.17g", v) end
scene("snap", function(animator, j1)
	local c = Instance.new("CurveAnimation") c.Loop = false
	posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(2, 2000, L)})
	local t = load(animator, c)
	t:Play(0, 1, 0)
	for _, k in {3, 27, 45} do
		local row = {}
		for _, e in {0, 1e-7, 3e-7, 1e-6, 2e-6, 3e-6, 5e-6, 1e-5, 2e-5, 3e-5, 5e-5, 1e-4, 2e-4, 5e-4, 1e-3, 2e-3, 5e-3, 1e-2, 0.1, 0.5, 0.9, 0.99, 0.995, 0.998, 0.999, 0.9995, 0.9998, 0.9999, 0.99995, 0.99998, 0.99999, 0.999995, 0.999998, 0.999999, 0.9999997} do
			t.TimePosition = (k + e) / 30 animator:StepAnimations(0)
			table.insert(row, e .. ":" .. G(t.TimePosition) .. "=" .. S(j1.Transform.X))
		end
		P("snap", k, table.concat(row, " "))
	end
end)
end
return table.concat(out, "\n")
