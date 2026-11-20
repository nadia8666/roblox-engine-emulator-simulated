-- A KeyframeSequence's track stepped with Animator:StepAnimations, as playback_steps.luau and playback_deltas.luau step
-- a CurveAnimation's: its time, what it fires and when it ends, forwards and backwards, looped and not, landing on
-- keyframes and on the clip's ends, a clip of Length 0, deltas of every kind and TimePosition set. Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode;
-- sequence_steps.txt is what Studio 0.747 returned (2026-11-11).
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
local function watch(track, names)
	local log = {}
	track.KeyframeReached:Connect(function(n) table.insert(log, "K:" .. tostring(n)) end)
	track.DidLoop:Connect(function() table.insert(log, "Loop") end)
	track.Stopped:Connect(function() table.insert(log, "Stopped") end)
	track.Ended:Connect(function() table.insert(log, "Ended") end)
	for _, n in names or {} do track:GetMarkerReachedSignal(n):Connect(function(v) table.insert(log, "M:" .. n .. "=" .. typeof(v) .. ":" .. tostring(v)) end) end
	return function() task.wait() local s = table.concat(log, ",") table.clear(log) return "<" .. s .. ">" end
end
local function state(track, j) return S(track.TimePosition) .. " " .. tostring(track.IsPlaying) .. " x=" .. S(j.Transform.X) end
local function marker(parent, name, list) local m = Instance.new("MarkerCurve") m.Name = name for _, e in list do m:InsertMarkerAtTime(e[1], e[2]) end m.Parent = parent return m end
local function ks(loop, list, zero)
	local s = Instance.new("KeyframeSequence") s.Loop = loop s.Priority = Enum.AnimationPriority.Action
	for _, e in list do
		local kf = Instance.new("Keyframe") kf.Time = e[1] if e[3] then kf.Name = e[3] end
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
		local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = CFrame.new(e[2], 0, 0) arm.Parent = root
		if e[4] then local m = Instance.new("KeyframeMarker") m.Name = "M" m.Value = e[4] m.Parent = kf end
		kf.Parent = s
	end
	return s
end
local function line(loop) return ks(loop, {{0, 0}, {0.25, 2.5, nil, "a"}, {0.75, 7.5, nil, "b"}, {1, 10, "Last"}}) end
local function cline(loop) local c = Instance.new("CurveAnimation") c.Loop = loop posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(1, 10, L)}) marker(c, "M", {{0.25, "a"}, {0.75, "b"}}) return c end
for _, kind in {"ks"} do
	for _, loop in {false, true} do
		local function make() return if kind == "ks" then line(loop) else cline(loop) end
		local tag = kind .. (if loop then "-loop" else "-once")
		scene(tag, function(animator, j1)
			local t = load(animator, make()) local ev = watch(t, {"M"})
			t:Play(0, 1, 1) animator:StepAnimations(0) P(tag, "fwd start", state(t, j1), ev())
			for i = 1, 5 do animator:StepAnimations(0.3) P(tag, "fwd", i, state(t, j1), ev()) end
		end)
		scene(tag, function(animator, j1)
			local t = load(animator, make()) local ev = watch(t, {"M"})
			t:Play(0, 1, -1) animator:StepAnimations(0) P(tag, "rev start", state(t, j1), ev())
			for i = 1, 5 do animator:StepAnimations(0.3) P(tag, "rev", i, state(t, j1), ev()) end
		end)
		scene(tag, function(animator, j1)
			local t = load(animator, make()) local ev = watch(t, {"M"})
			t:Play(0, 1, -1) animator:StepAnimations(0.3) P(tag, "rev first 0.3", state(t, j1), ev())
			animator:StepAnimations(0.3) P(tag, "rev second", state(t, j1), ev())
		end)
		scene(tag, function(animator, j1)
			local t = load(animator, make()) local ev = watch(t, {"M"})
			t:Play(0, 1, 1) animator:StepAnimations(0.5) P(tag, "exact half", state(t, j1), ev())
			animator:StepAnimations(0.5) P(tag, "exact end", state(t, j1), ev())
			animator:StepAnimations(0.25) P(tag, "after", state(t, j1), ev())
		end)
		scene(tag, function(animator, j1)
			local t = load(animator, make()) local ev = watch(t, {"M"})
			t:Play(0, 1, 1) t.TimePosition = 0.7 animator:StepAnimations(0) ev() animator:StepAnimations(0.65) P(tag, "wrap order", state(t, j1), ev())
			t:AdjustSpeed(-1) t.TimePosition = 0.3 animator:StepAnimations(0) ev() animator:StepAnimations(0.65) P(tag, "wrap order back", state(t, j1), ev())
		end)
		scene(tag, function(animator, j1)
			local t = load(animator, make()) local ev = watch(t, {"M"})
			t:Play(0, 1, 0) animator:StepAnimations(0.1) P(tag, "speed 0", state(t, j1), ev())
			t.TimePosition = 1 animator:StepAnimations(0.1) P(tag, "speed 0 set end", state(t, j1), ev())
			t.TimePosition = 0.25 animator:StepAnimations(0.1) P(tag, "speed 0 set marker", state(t, j1), ev())
			t:AdjustSpeed(1) animator:StepAnimations(0.1) P(tag, "then speed 1", state(t, j1), ev())
			animator:StepAnimations(-0.2) P(tag, "negative delta", state(t, j1), ev())
			animator:StepAnimations(3.5) P(tag, "long step", state(t, j1), ev())
		end)
	end
end
-- a marker at 0: when it fires
for _, kind in {"ks"} do
	scene("start", function(animator, j1)
		local clip
		if kind == "ks" then clip = ks(false, {{0, 0, "First", "zero"}, {1, 10, "Last", "one"}}) else clip = Instance.new("CurveAnimation") clip.Loop = false posX(folder("Arm", folder("Root", clip)), {FK(0, 0, L), FK(1, 10, L)}) marker(clip, "M", {{0, "zero"}, {1, "one"}}) end
		local t = load(animator, clip) local ev = watch(t, {"M"})
		t:Play(0, 1, 1) animator:StepAnimations(0) P("start", kind, "step 0", state(t, j1), ev())
		animator:StepAnimations(0.1) P("start", kind, "step 0.1", state(t, j1), ev())
		animator:StepAnimations(2) P("start", kind, "to end", state(t, j1), ev())
		t:Play(0, 1, 1) animator:StepAnimations(0.1) P("start", kind, "replay 0.1", state(t, j1), ev())
		t:Stop(0) animator:StepAnimations(0.1) ev()
		t:Play(0, 1, 0) animator:StepAnimations(0.1) P("start", kind, "speed 0 step 0.1", state(t, j1), ev())
		t:AdjustSpeed(1) animator:StepAnimations(0.1) P("start", kind, "speed 1 step 0.1", state(t, j1), ev())
	end)
end
-- Length 0, backwards and for a KeyframeSequence
for _, kind in {"ks"} do
	for _, speed in {1, -1} do
		scene("zero", function(animator, j1)
			local clip
			if kind == "ks" then clip = ks(false, {{0, 7, "Only", "zero"}}) else clip = Instance.new("CurveAnimation") clip.Loop = false posX(folder("Arm", folder("Root", clip)), {FK(0, 7, L)}) marker(clip, "M", {{0, "zero"}}) end
			local id = ACP:RegisterAnimationClip(clip)
			local animation = Instance.new("Animation") animation.AnimationId = id
			local t = animator:LoadAnimation(animation) for i = 1, 10 do task.wait() end
			local ev = watch(t, {"M"})
			P("zero", kind, speed, "length", t.Length)
			t:Play(0, 1, speed) animator:StepAnimations(0) P("zero", kind, speed, "start", state(t, j1), ev())
			for i = 1, 3 do animator:StepAnimations(0.1) P("zero", kind, speed, i, state(t, j1), ev()) end
			t.TimePosition = 5 animator:StepAnimations(0.1) P("zero", kind, speed, "set 5", state(t, j1), ev())
			t:Stop(0) animator:StepAnimations(0.1) P("zero", kind, speed, "stopped", state(t, j1), ev())
			t:Play(0, 1, speed) animator:StepAnimations(0.1) P("zero", kind, speed, "replay", state(t, j1), ev())
		end)
	end
end
do
local KS = true
local function mk(loop, ks)
	if ks then
		local s = Instance.new("KeyframeSequence") s.Loop = loop
		for _, e in {{0, "a"}, {0.25, "b"}, {0.5, "c"}, {1, "d"}} do
			local kf = Instance.new("Keyframe") kf.Time = e[1] kf.Name = "k" .. e[2]
			local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
			local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = CFrame.new(e[1] * 10, 0, 0) arm.Parent = root
			local m = Instance.new("KeyframeMarker") m.Name = e[2] m.Value = e[2] m.Parent = kf
			kf.Parent = s
		end
		return s
	end
	local c = Instance.new("CurveAnimation") c.Loop = loop
	posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(1, 10, L)})
	marker(c, "a", {{0, "a"}}) marker(c, "b", {{0.25, "b"}}) marker(c, "c", {{0.5, "c"}}) marker(c, "d", {{1, "d"}})
	return c
end
local function run(label, loop, speed, steps)
	scene(label, function(animator, j1)
		local t = load(animator, mk(loop, KS))
		local read = watch(t, {"a", "b", "c", "d"})
		t:Play(0, 1, speed)
		local row = {}
		for _, d in steps do
			local what
			if type(d) == "table" then t.TimePosition = d[1] animator:StepAnimations(0) what = "set " .. d[1] else animator:StepAnimations(d) what = tostring(d) end
			row[#row + 1] = string.format("%s: %.9g x=%.9g w=%.9g %s %s", what, t.TimePosition, j1.Transform.X, t.WeightCurrent, t.IsPlaying and "p" or "-", read())
		end
		P(label) for _, r in row do P("  " .. r) end
	end)
end
run("L1 looped", true, 1, {0.2, 2.5})
run("L2 looped", true, 1, {0.2, 2.05})
run("L3 looped", true, 1, {0, 3})
run("L4 looped back", true, -1, {0.3, -0.5})
run("L5 looped back", true, -1, {0.3, 2.5})
run("N1 once", false, 1, {0.2, -0.5, 0.1})
run("N2 once back", false, -1, {0.3, -0.5, 0.1})
run("N3 once back", false, -1, {0.3, -0.1, 0.1})
run("T1 looped set", true, 1, {0.2, {-0.3}, 0.1, {-2.5}, 0.1, {7.25}, 0.1})
run("T2 looped back set", true, -1, {0.2, {-0.3}, 0.1, {7.25}, 0.1})
run("T3 once set", false, 1, {0.2, {-0.3}, 0.1, {5}, 0.1})
run("T4 once back set", false, -1, {0.2, {5}, 0.1, {-0.3}, 0.1})
end
do
local KS = true
local function mk(loop, ks)
	if ks then
		local s = Instance.new("KeyframeSequence") s.Loop = loop
		for _, e in {{0, "a"}, {0.25, "b"}, {0.5, "c"}, {1, "d"}} do
			local kf = Instance.new("Keyframe") kf.Time = e[1] kf.Name = "k" .. e[2]
			local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
			local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = CFrame.new(e[1] * 10, 0, 0) arm.Parent = root
			local m = Instance.new("KeyframeMarker") m.Name = e[2] m.Value = e[2] m.Parent = kf
			kf.Parent = s
		end
		return s
	end
	local c = Instance.new("CurveAnimation") c.Loop = loop
	posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(1, 10, L)})
	marker(c, "a", {{0, "a"}}) marker(c, "b", {{0.25, "b"}}) marker(c, "c", {{0.5, "c"}}) marker(c, "d", {{1, "d"}})
	return c
end
local function run(label, loop, steps, fade)
	scene(label, function(animator, j1)
		local t = load(animator, mk(loop, KS))
		local read = watch(t, {"a", "b", "c", "d"})
		t:Play(fade or 0)
		local row = {}
		for _, d in steps do
			animator:StepAnimations(d)
			row[#row + 1] = string.format("%s: %.9g x=%.9g w=%.9g %s %s", tostring(d), t.TimePosition, j1.Transform.X, t.WeightCurrent, t.IsPlaying and "p" or "-", read())
		end
		P(label) for _, r in row do P("  " .. r) end
	end)
end
run("S1 looped", true, {0.35, -0.2, 0.1, 0.1, 0.1, 3.5, 0.1})
run("S2 looped", true, {0.5, -0.1, -0.1, 0.05, 0.3})
run("S3 looped", true, {-0.1, 0.1, 0.2})
run("S4 once", false, {0.35, -0.2, 0.5, 0.5, 0.1})
run("S5 nan", true, {0.35, 0 / 0, 0.1, 0.1})
run("S6 inf", true, {0.35, math.huge, 0.1, 0.1})
run("S7 fade", true, {0.05, -0.02, 0.05, -0.1, 0.05, 0.05}, 0.2)
run("S8 -inf", true, {0.35, -math.huge, 0.1})
run("S9 looped fwd wrap", true, {0.9, 0.2, 0.9, 0.1})
run("S10 once fwd end", false, {0.9, 0.2, 0.1})
end
return table.concat(out, "\n")
