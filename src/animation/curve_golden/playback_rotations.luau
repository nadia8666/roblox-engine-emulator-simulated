-- The rotations a CurveAnimation's track gives a joint: forty constant rotations about X, about other axes and of two
-- turns, each read from the joint's Transform. Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode; playback_rotations.txt is what Studio 0.747 returned
-- (2026-11-11). Compared within 1e-6 an entry: Studio's own float32 sine and cosine leave the last digit open.
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
local function M(cf) local t = {cf:GetComponents()} local o = {} for i = 4, 12 do o[#o + 1] = string.format("%.9g", t[i]) end return table.concat(o, ",") end
scene("const", function(animator, j1)
	for i = 0, 119, 3 do
		local key
		if i < 70 then key = CFrame.Angles(i * 0.09 - 3.1, 0, 0)
		elseif i < 100 then key = CFrame.fromAxisAngle(Vector3.new(math.sin(i), math.cos(i * 1.7), math.sin(i * 0.3 + 1)).Unit, (i - 69) * 0.1)
		else key = CFrame.Angles(0, (i - 100) * 0.21 - 3, 0) * CFrame.Angles(0, 0, (i - 100) * 0.1) end
		local c2 = Instance.new("CurveAnimation") c2.Loop = false
		local r = Instance.new("RotationCurve") r.Name = "Rotation" r.Parent = folder("Arm", folder("Root", c2))
		r:InsertKey(RotationCurveKey.new(0, key, L)) r:InsertKey(RotationCurveKey.new(1, key, L))
		local t2 = load(animator, c2) t2:Play(0, 1, 0) t2.TimePosition = 0.5 animator:StepAnimations(0)
		P("i", i, M(r:GetKeys()[1].Value), M(j1.Transform))
		t2:Stop(0) animator:StepAnimations(0) t2:Destroy()
	end
end)
return table.concat(out, "\n")
