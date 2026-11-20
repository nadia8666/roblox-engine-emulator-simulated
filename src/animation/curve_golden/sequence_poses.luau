-- The poses a KeyframeSequence's track gives a joint: clips played from their keys and clips Studio plays from a packed
-- form (evenly keyed, every Pose Linear), which of the two a clip is, every easing style and direction, rotations, and
-- the times of a packed clip's keyframes. Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode; sequence_poses.txt is what Studio 0.747 returned
-- (2026-11-11). Compared within 2e-6 a number: Studio's own float32 sine, cosine and powers leave the last digit open.
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
local function ks(keys)
	local s = Instance.new("KeyframeSequence") s.Loop = false
	for _, e in keys do
		local kf = Instance.new("Keyframe") kf.Time = e[1]
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
		local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = CFrame.new(e[2], e[3] or 0, 0) arm.Parent = root
		kf.Parent = s
	end
	return s
end
local sets = {
	A = {{0, 0}, {1, 10}},
	B = {{0, 0}, {0.25, 2.5}, {1, 10}},
	C = {{0, 1000, 3}, {1, 1000, 7}},
	D = {{0, 3}, {0.7, 11}, {1.3, -4}},
}
scene("ks", function(animator, j1)
	for _, name in {"A", "B", "C", "D"} do
		local t = load(animator, ks(sets[name]))
		t:Play(0, 1, 0) animator:StepAnimations(0)
		local row = {}
		for i = 1, 45 do
			local time = (i * 0.0219 + (i % 7) * 0.0031) % t.Length
			t.TimePosition = time animator:StepAnimations(0)
			row[#row + 1] = string.format("%.9g=%.9g,%.9g", t.TimePosition, j1.Transform.X, j1.Transform.Y)
		end
		P(name, table.concat(row, " "))
		t:Stop(0) animator:StepAnimations(0)
	end
end)
end
do
local function ks(keys)
	local s = Instance.new("KeyframeSequence") s.Loop = false
	for _, e in keys do
		local kf = Instance.new("Keyframe") kf.Time = e[1]
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
		local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = e[2] arm.Parent = root
		kf.Parent = s
	end
	return s
end
local function V(v) return string.format("%.9g,%.9g,%.9g", v.X, v.Y, v.Z) end
scene("ks", function(animator, j1)
	local function try(label, keys, times)
		local t = load(animator, ks(keys))
		t:Play(0, 1, 0) animator:StepAnimations(0)
		local row = {}
		for _, time in times do
			t.TimePosition = time animator:StepAnimations(0)
			row[#row + 1] = string.format("%.9g=%s", t.TimePosition, V(j1.Transform.Position))
		end
		P(label, table.concat(row, "  "))
		t:Stop(0) animator:StepAnimations(0)
	end
	for _, x in {0, 1, 10, 100, 1000, 5000, -1000} do
		try("x" .. x, {{0, CFrame.new(x, 3, 0)}, {1, CFrame.new(x, 7, 0)}}, {0, 0.5, 0.999})
	end
	try("three", {{0, CFrame.new(1000, 3, 0)}, {0.5, CFrame.new(1000, 5, 0)}, {1, CFrame.new(1000, 7, 0)}}, {0, 0.25, 0.5, 0.75, 0.999})
	try("four", {{0, CFrame.new(1000, 3, 0)}, {0.25, CFrame.new(1000, 4, 0)}, {0.5, CFrame.new(1000, 5, 0)}, {1, CFrame.new(1000, 7, 0)}}, {0, 0.25, 0.5, 0.75, 0.999})
	try("big steps", {{0, CFrame.new(1000, 3, 0)}, {0.5, CFrame.new(1000, 500, 0)}, {1, CFrame.new(1000, 7, 0)}}, {0, 0.25, 0.5, 0.75, 0.999})
	try("const", {{0, CFrame.new(1000, 3, 0)}, {1, CFrame.new(1000, 3, 0)}}, {0, 0.5, 0.999})
	try("small", {{0, CFrame.new(0.001, 0.003, 0)}, {1, CFrame.new(0.001, 0.007, 0)}}, {0, 0.5, 0.999})
	try("b", {{0, CFrame.new(0, 0, 0)}, {0.25, CFrame.new(2.5, 0, 0)}, {1, CFrame.new(10, 0, 0)}}, {0, 0.25, 0.5, 0.999})
	try("b2", {{0, CFrame.new(0, 0, 0)}, {0.25, CFrame.new(2.7, 0, 0)}, {1, CFrame.new(10, 0, 0)}}, {0, 0.25, 0.5, 0.999})
	try("looped time", {{0, CFrame.new(0, 3, 0)}, {10, CFrame.new(0, 7, 0)}}, {0, 5, 9.99})
end)
end
do
local function ks(times, arm, hand, skipHand)
	local s = Instance.new("KeyframeSequence") s.Loop = false
	for i, time in times do
		local kf = Instance.new("Keyframe") kf.Time = time
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
		local a = Instance.new("Pose") a.Name = "Arm" a.CFrame = arm(i, time) a.Parent = root
		if hand and not (skipHand and skipHand[i]) then local h = Instance.new("Pose") h.Name = "Hand" h.CFrame = hand(i, time) h.Parent = a end
		kf.Parent = s
	end
	return s
end
local function V(v) return string.format("%.9g,%.9g,%.9g", v.X, v.Y, v.Z) end
local function M(cf) local t = {cf:GetComponents()} local o = {} for i = 4, 12 do o[#o + 1] = string.format("%.9g", t[i]) end return table.concat(o, ",") end
scene("ks", function(animator, j1, j2)
	local function try(label, clip, times, rot)
		local t = load(animator, clip)
		t:Play(0, 1, 0) animator:StepAnimations(0)
		local row = {}
		for _, time in times do
			t.TimePosition = time animator:StepAnimations(0)
			row[#row + 1] = rot and M(j1.Transform) or string.format("%.9g=%s|%s", t.TimePosition, V(j1.Transform.Position), V(j2.Transform.Position))
		end
		P(label, table.concat(row, "  "))
		t:Stop(0) animator:StepAnimations(0)
	end
	local big = function(i, time) return CFrame.new(1000, 3 + 4 * time, 0) end
	try("0,1", ks({0, 1}, big), {0})
	try("0,.5,1", ks({0, 0.5, 1}, big), {0})
	try("0,.25,.5,1", ks({0, 0.25, 0.5, 1}, big), {0})
	try("5 even", ks({0, 0.25, 0.5, 0.75, 1}, big), {0})
	try("offset even", ks({0.1, 0.6, 1.1}, big), {0.1, 0.35})
	try("0,.5,1.01", ks({0, 0.5, 1.01}, big), {0})
	try("thirds", ks({0, 0.3333, 0.6667, 1}, big), {0})
	try("thirds exact", ks({0, 1 / 3, 2 / 3, 1}, big), {0})
	try("0,1,2,3", ks({0, 1, 2, 3}, big), {0})
	try("30 fps 7", ks({0, 1 / 30, 2 / 30, 3 / 30, 4 / 30, 5 / 30, 6 / 30}, big), {0})
	try("0,.1,.2,.4", ks({0, 0.1, 0.2, 0.4}, big), {0})
	try("one key", ks({0}, big), {0})
	try("two joints", ks({0, 1}, big, function(i, time) return CFrame.new(0, 3 + 4 * time, 0) end), {0, 0.5})
	try("two joints small arm", ks({0, 1}, function(i, time) return CFrame.new(0, 3 + 4 * time, 0) end, function(i, time) return CFrame.new(1000, 3 + 4 * time, 0) end), {0, 0.5})
	try("hand missing mid", ks({0, 0.5, 1}, big, function(i, time) return CFrame.new(1000, 3 + 4 * time, 0) end, {[2] = true}), {0, 0.25})
	try("negative", ks({0, 1}, function(i, time) return CFrame.new(1000, -3 - 4 * time, -0.5) end), {0, 0.5})
	try("mixed sign", ks({0, 1}, function(i, time) return CFrame.new(-1000 + 1500 * time, 3, 0) end), {0, 0.5, 0.999})
	for i = 0, 11 do
		local cf = i < 8 and CFrame.Angles(0.37 * i - 1.2, 0, 0) or CFrame.fromAxisAngle(Vector3.new(i, 2, -1).Unit, i * 0.3)
		try("rot" .. i, ks({0, 1}, function() return cf end), {0.5}, true)
		P("key" .. i, M(cf))
	end
end)
end
do
local function ks(times, set)
	local s = Instance.new("KeyframeSequence") s.Loop = false
	for i, time in times do
		local kf = Instance.new("Keyframe") kf.Time = time
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
		local a = Instance.new("Pose") a.Name = "Arm" a.CFrame = CFrame.new(1000, 3 + i, 0) a.Parent = root
		local h = Instance.new("Pose") h.Name = "Hand" h.CFrame = CFrame.new(1000, 3 + i, 0) h.Parent = a
		if set then set(root, a, h, i) end
		kf.Parent = s
	end
	return s
end
scene("ks", function(animator, j1, j2)
	local function try(label, clip)
		local t = load(animator, clip)
		t:Play(0, 1, 0) animator:StepAnimations(0)
		P(label, string.format("%.9g %.9g", j1.Transform.Y, j2.Transform.Y))
		t:Stop(0) animator:StepAnimations(0)
	end
	try("drift 1,1.05,1.1,1.1,1.1", ks({0, 1, 2.05, 3.15, 4.25, 5.35}))
	try("0,1,2.1,3.2", ks({0, 1, 2.1, 3.2}))
	try("0,1,2,3.1", ks({0, 1, 2, 3.1}))
	try("0,1,2,3.15", ks({0, 1, 2, 3.15}))
	try("0,1,2.05", ks({0, 1, 2.05}))
	try("0,1,2,3,4.1", ks({0, 1, 2, 3, 4.1}))
	try("0,1,2,3,4,5,6,7,8.1", ks({0, 1, 2, 3, 4, 5, 6, 7, 8.1}))
	try("0,1.1,2.1,3.1", ks({0, 1.1, 2.1, 3.1}))
	try("0,1,2.11", ks({0, 1, 2.11}))
	try("0,1,2.105", ks({0, 1, 2.105}))
	try("0,1,2.095", ks({0, 1, 2.095}))
	try("hand cubic", ks({0, 1, 2}, function(root, a, h) h.EasingStyle = Enum.PoseEasingStyle.Cubic end))
	try("hand cubic in last key only", ks({0, 1, 2}, function(root, a, h, i) if i == 3 then h.EasingStyle = Enum.PoseEasingStyle.Cubic end end))
	try("hand cubic in first key only", ks({0, 1, 2}, function(root, a, h, i) if i == 1 then h.EasingStyle = Enum.PoseEasingStyle.Cubic end end))
	try("direction Out", ks({0, 1, 2}, function(root, a, h) h.EasingDirection = Enum.PoseEasingDirection.Out a.EasingDirection = Enum.PoseEasingDirection.InOut end))
	try("root constant", ks({0, 1, 2}, function(root, a, h) root.EasingStyle = Enum.PoseEasingStyle.Constant end))
	try("hand weight 0", ks({0, 1, 2}, function(root, a, h) h.Weight = 0 end))
	try("hand weight 0.5", ks({0, 1, 2}, function(root, a, h) h.Weight = 0.5 end))
	try("hand weight 0 in key 2", ks({0, 1, 2}, function(root, a, h, i) if i == 2 then h.Weight = 0 end end))
	try("two keyframes at one time", ks({0, 1, 1, 2}))
	try("first key at 0.5", ks({0.5, 1.5, 2.5}))
end)
end
do
local function ks(keys, style, direction)
	local s = Instance.new("KeyframeSequence") s.Loop = false
	for _, e in keys do
		local kf = Instance.new("Keyframe") kf.Time = e[1] if e[3] then kf.Name = e[3] end
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
		local a = Instance.new("Pose") a.Name = "Arm" a.CFrame = e[2] a.Parent = root
		if style then a.EasingStyle = style a.EasingDirection = direction or Enum.PoseEasingDirection.In end
		if e[4] then local m = Instance.new("KeyframeMarker") m.Name = "M" m.Value = e[4] m.Parent = kf end
		kf.Parent = s
	end
	return s
end
local function M(cf) local t = {cf:GetComponents()} local o = {} for i = 1, 12 do o[#o + 1] = string.format("%.9g", t[i]) end return table.concat(o, ",") end
scene("ks", function(animator, j1)
	local function try(label, clip, times)
		local t = load(animator, clip)
		t:Play(0, 1, 0) animator:StepAnimations(0)
		for _, time in times do
			t.TimePosition = time animator:StepAnimations(0)
			P(label, time, M(j1.Transform))
		end
		t:Stop(0) animator:StepAnimations(0)
	end
	local times = {0.1, 0.25, 0.5, 0.75, 0.9}
	try("turn", ks({{0, CFrame.Angles(0, 0, 0)}, {1, CFrame.Angles(2, 0, 0)}}), times)
	try("turn2", ks({{0, CFrame.Angles(0.3, 0.2, 0)}, {1, CFrame.Angles(-1, 1.4, 0.6)}}), times)
	try("move", ks({{0, CFrame.new(3.3, 0, -1)}, {1, CFrame.new(7.9, 2, 5)}}), times)
	for _, style in {"Constant", "Cubic", "Elastic", "Bounce", "CubicV2"} do
		for _, direction in {"In", "Out", "InOut"} do
			try(style .. direction, ks({{0, CFrame.new(0, 0, 0)}, {1, CFrame.new(10, 0, 0) * CFrame.Angles(1, 0, 0)}}, Enum.PoseEasingStyle[style], Enum.PoseEasingDirection[direction]), {0.25, 0.6})
		end
	end
end)
scene("names", function(animator, j1)
	local t = load(animator, ks({{0, CFrame.new(0, 0, 0), "A"}, {0.5, CFrame.new(5, 0, 0), "B", "b"}, {1.05, CFrame.new(10, 0, 0), "C", "c"}}))
	local log = {}
	t.KeyframeReached:Connect(function(n) log[#log + 1] = "K:" .. n end)
	t:GetMarkerReachedSignal("M"):Connect(function(v) log[#log + 1] = "M:" .. v end)
	P("names", "Length", t.Length, "B", t:GetTimeOfKeyframe("B"), "C", t:GetTimeOfKeyframe("C"))
	t:Play(0, 1, 1) animator:StepAnimations(0)
	local row = {}
	for i = 1, 24 do animator:StepAnimations(0.05) task.wait() if #log > 0 then row[#row + 1] = string.format("%.9g:%s", t.TimePosition, table.concat(log, ",")) table.clear(log) end end
	P("names fired", table.concat(row, " "))
end)
end
do
local function ks(keys, style, direction)
	local s = Instance.new("KeyframeSequence") s.Loop = false
	for _, e in keys do
		local kf = Instance.new("Keyframe") kf.Time = e[1] if e[3] then kf.Name = e[3] end
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
		local a = Instance.new("Pose") a.Name = "Arm" a.CFrame = e[2] a.Parent = root
		if style then a.EasingStyle = style a.EasingDirection = direction or Enum.PoseEasingDirection.In end
		if e[4] then local m = Instance.new("KeyframeMarker") m.Name = "M" m.Value = e[4] m.Parent = kf end
		kf.Parent = s
	end
	return s
end
local function M(cf) local t = {cf:GetComponents()} local o = {} for i = 1, 12 do o[#o + 1] = string.format("%.9g", t[i]) end return table.concat(o, ",") end
scene("ks", function(animator, j1)
	local function try(label, clip, times)
		local t = load(animator, clip)
		t:Play(0, 1, 0) animator:StepAnimations(0)
		for _, time in times do
			t.TimePosition = time animator:StepAnimations(0)
			P(label, time, M(j1.Transform))
		end
		t:Stop(0) animator:StepAnimations(0)
	end
	for _, style in {"Constant", "Cubic", "Linear"} do
		for _, direction in {"In", "Out", "InOut"} do
			try("uneven " .. style .. direction, ks({{0, CFrame.new(0, 0, 0)}, {0.25, CFrame.new(10, 0, 0) * CFrame.Angles(1, 0, 0)}, {1, CFrame.new(20, 0, 0)}}, Enum.PoseEasingStyle[style], Enum.PoseEasingDirection[direction]), {0.0625, 0.15, 0.25, 0.4, 0.7})
		end
	end
end)
end
return table.concat(out, "\n")
