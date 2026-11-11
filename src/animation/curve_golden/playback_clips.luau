-- CurveAnimations at the edges: keys before 0 s, a clip of Length 0, markers of other values, an id registered for
-- what is no clip, and what a registered id stands for. Stepped with Animator:StepAnimations. Runs unchanged in
-- Studio (the command bar or a plugin, in an edit session) and here in edit mode; playback_clips.txt is what Studio
-- 0.747 returned (2026-11-11). Poses are printed to four decimals.
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
local function state(track, j) return S(track.TimePosition) .. " " .. tostring(track.IsPlaying) .. " x=" .. S(math.round(j.Transform.X * 1e4) / 1e4 + 0) end
local function marker(parent, name, list) local m = Instance.new("MarkerCurve") m.Name = name for _, e in list do m:InsertMarkerAtTime(e[1], e[2]) end m.Parent = parent return m end
local function clip(loop, markers, keys)
	local c = Instance.new("CurveAnimation") c.Loop = loop
	posX(folder("Arm", folder("Root", c)), keys or {FK(0, 0, L), FK(1, 10, L)})
	marker(c, "M", markers)
	return c
end
local function run(tag, c, speed, steps, before)
	scene(tag, function(animator, j1)
		local t = load(animator, c) local ev = watch(t, {"M"})
		t:Play(0, 1, speed)
		if before then before(t) end
		for i, dt in steps do
			if type(dt) == "table" then t.TimePosition = dt[1] dt = dt[2] end
			animator:StepAnimations(dt) P(tag, i, state(t, j1), ev())
		end
	end)
end
-- negative-time keys
scene("neg-keys", function(animator, j1)
	local c = Instance.new("CurveAnimation") c.Loop = false
	posX(folder("Arm", folder("Root", c)), {FK(-1, -10, L), FK(0, 0, L), FK(1, 10, L)})
	marker(c, "M", {{-0.5, "before"}, {0, "zero"}, {0.5, "half"}})
	local t = load(animator, c) local ev = watch(t, {"M"})
	P("neg-keys length", t.Length, c.Length)
	t:Play(0, 1, 0) animator:StepAnimations(0) P("neg-keys at0", state(t, j1), ev())
	for _, tp in {-0.5, -2, 0.25, 0.75, 1, 5} do t.TimePosition = tp animator:StepAnimations(0) P("neg-keys set", tp, state(t, j1), ev()) end
end)
scene("only-neg", function(animator, j1)
	local c = Instance.new("CurveAnimation") c.Loop = false
	posX(folder("Arm", folder("Root", c)), {FK(-2, -20, L), FK(-1, -10, L)})
	local t = load(animator, c) local ev = watch(t)
	P("only-neg length", t.Length, c.Length)
	t:Play(0, 1, 1) animator:StepAnimations(0) P("only-neg at0", state(t, j1), ev())
	animator:StepAnimations(0.1) P("only-neg step", state(t, j1), ev())
	animator:StepAnimations(0.1) P("only-neg step", state(t, j1), ev())
end)
scene("late-keys", function(animator, j1)
	local c = Instance.new("CurveAnimation") c.Loop = false
	posX(folder("Arm", folder("Root", c)), {FK(1, 10, L), FK(2, 20, L)})
	local t = load(animator, c) local ev = watch(t)
	P("late length", t.Length)
	t:Play(0, 1, 0) for _, tp in {0, 0.5, 1, 1.5, 2} do t.TimePosition = tp animator:StepAnimations(0) P("late set", tp, state(t, j1)) end
end)
-- Length 0, backwards and for a KeyframeSequence
for _, kind in {"curve"} do
	for _, speed in {1, -1} do
		scene("zero", function(animator, j1)
			local clip
			clip = Instance.new("CurveAnimation") clip.Loop = false posX(folder("Arm", folder("Root", clip)), {FK(0, 7, L)}) marker(clip, "M", {{0, "zero"}})
			local id = ACP:RegisterAnimationClip(clip)
			local animation = Instance.new("Animation") animation.AnimationId = id
			local t = animator:LoadAnimation(animation) for i = 1, 10 do task.wait() end
			local ev = watch(t, {"M"})
			P("zero", kind, speed, "length", t.Length)
			t:Play(0, 1, speed) animator:StepAnimations(0) P("zero", kind, speed, "start", state(t, j1), ev())
			for i = 1, 3 do animator:StepAnimations(0.1) P("zero", kind, speed, i, state(t, j1), ev()) end
			t.TimePosition = 5 animator:StepAnimations(0.1) P("zero", kind, speed, "set 5", state(t, j1), ev())
			t:Stop(0) animator:StepAnimations(0.1) P("zero", kind, speed, "stopped", S(t.TimePosition), t.IsPlaying, ev())
			t:Play(0, 1, speed) animator:StepAnimations(0.1) P("zero", kind, speed, "replay", state(t, j1), ev())
		end)
	end
end
scene("empty", function(animator, j1)
	local c = Instance.new("CurveAnimation")
	local ok, id = pcall(function() return ACP:RegisterAnimationClip(c) end)
	P("empty register", ok, if ok then type(id) else id)
	if not ok then return end
	local animation = Instance.new("Animation") animation.AnimationId = id
	local t = animator:LoadAnimation(animation) for i = 1, 10 do task.wait() end
	local ev = watch(t)
	P("empty length", t.Length)
	t:Play(0, 1, 1) animator:StepAnimations(0) P("empty start", state(t, j1), ev())
	animator:StepAnimations(0.1) P("empty step", state(t, j1), ev())
end)
run("zero-markers", clip(false, {{-0.5, "before"}, {0, "zero"}, {0.5, "half"}}, {FK(0, 7, L)}), 1, {0.3, 0.3, 0.3})
run("zero-markers-back", clip(false, {{-0.5, "before"}, {0, "zero"}, {0.5, "half"}}, {FK(0, 7, L)}), -1, {0.3, 0.3, 0.3})
run("zero-set", clip(true, {{0, "zero"}, {0.5, "half"}}, {FK(0, 7, L)}), 1, {{0.4, 0.2}, {-3, 0.2}})
run("neg-marker", clip(false, {{-0.5, "before"}, {0.5, "half"}}, {FK(-1, -10, L), FK(1, 10, L)}), -1, {0.75, 0.5})
scene("marker-fire", function(animator, j1)
	local c = Instance.new("CurveAnimation") c.Loop = false
	posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(1, 10, L)})
	local m = Instance.new("MarkerCurve") m.Name = "M" m:InsertMarkerAtTime(0.1, 5) m:InsertMarkerAtTime(0.2, true) m:InsertMarkerAtTime(0.3, 1.5) m.Parent = c
	local t = load(animator, c) local ev = watch(t, {"M"})
	t:Play(0, 1, 1) animator:StepAnimations(0) ev() animator:StepAnimations(0.5) P("marker-fire", ev())
end)
scene("non-clip", function(animator, j1)
	local made = Instance.new("Folder")
	local id = KSP:RegisterKeyframeSequence(made)
	local animation = Instance.new("Animation") animation.AnimationId = id
	local done = {}
	task.spawn(function() local ok, t = pcall(function() return animator:LoadAnimation(animation) end) done.load = if ok then t else "E(" .. tostring(t) .. ")" end)
	task.spawn(function() done.ks = try(function() local got = KSP:GetKeyframeSequenceAsync(id) return got.ClassName, #got:GetChildren(), got == made end) end)
	task.spawn(function() done.clip = try(function() local got = ACP:GetAnimationClipAsync(id) return got.ClassName, got == made end) end)
	for i = 1, 30 do task.wait() end
	P("non-clip load", typeof(done.load), tostring(done.load), "ks", tostring(done.ks), "clip", tostring(done.clip))
	local t = done.load
	P("non-clip register", try(function() return type(KSP:RegisterKeyframeSequence(Instance.new("Part"))) end), try(function() return ACP:RegisterAnimationClip(made) end), try(function() return KSP:RegisterKeyframeSequence() end), try(function() return KSP:RegisterKeyframeSequence(5) end), try(function() return ACP:RegisterAnimationClip() end), try(function() return ACP:RegisterAnimationClip("x") end))
	if typeof(t) == "Instance" then
		local ev = watch(t)
		P("non-clip track", t.Length, t.Looped, tostring(t.Priority), t.IsPlaying)
		t:Play(0, 1, 1) animator:StepAnimations(0.1) P("non-clip play", state(t, j1), ev())
		animator:StepAnimations(0.1) P("non-clip play", state(t, j1), ev())
		t:Stop(0) animator:StepAnimations(0.1) P("non-clip stop", state(t, j1), ev())
	end
end)
-- ids, and whether a registered clip is a copy
scene("ids", function(animator, j1)
	local c = clip(false, {})
	local x = c.Root.Arm.Position:X()
	local id1 = ACP:RegisterAnimationClip(c)
	local id1b = ACP:RegisterAnimationClip(c)
	local animation = Instance.new("Animation") animation.AnimationId = id1
	local early = animator:LoadAnimation(animation) for i = 1, 10 do task.wait() end
	x:SetKeys({FK(0, 0, L), FK(2, 40, L)})
	local id2 = ACP:RegisterAnimationClip(c)
	local twin = clip(false, {}) twin.Root.Arm.Position:X():SetKeys({FK(0, 0, L), FK(2, 40, L)})
	local id3 = ACP:RegisterAnimationClip(twin)
	local copy = c:Clone()
	local id4 = ACP:RegisterAnimationClip(copy)
	P("ids", "again", id1 == id1b, "changed", id1 == id2, "twin of changed", id2 == id3, "clone", id2 == id4, "ks", id2 == KSP:RegisterKeyframeSequence(c))
	early:Play(0, 1, 0) early.TimePosition = 0.5 animator:StepAnimations(0) P("ids early track", early.Length, S(j1.Transform.X)) early:Stop(0) animator:StepAnimations(0)
	local late = animator:LoadAnimation(animation) for i = 1, 10 do task.wait() end
	late:Play(0, 1, 0) late.TimePosition = 0.5 animator:StepAnimations(0) P("ids id1 loaded after change", late.Length, S(j1.Transform.X)) late:Stop(0) animator:StepAnimations(0)
	local a2 = Instance.new("Animation") a2.AnimationId = id2
	local t2 = animator:LoadAnimation(a2) for i = 1, 10 do task.wait() end
	t2:Play(0, 1, 0) t2.TimePosition = 0.5 animator:StepAnimations(0) P("ids id2", t2.Length, S(j1.Transform.X)) t2:Stop(0) animator:StepAnimations(0)
	-- active
	local live = clip(false, {})
	local act = ACP:RegisterActiveAnimationClip(live)
	local a3 = Instance.new("Animation") a3.AnimationId = act
	local t3 = animator:LoadAnimation(a3) for i = 1, 10 do task.wait() end
	live.Root.Arm.Position:X():SetKeys({FK(0, 0, L), FK(2, 40, L)})
	local t4 = animator:LoadAnimation(a3) for i = 1, 10 do task.wait() end
	t4:Play(0, 1, 0) t4.TimePosition = 0.5 animator:StepAnimations(0) P("ids active late track", t4.Length, S(j1.Transform.X)) t4:Stop(0) animator:StepAnimations(0)
	local sq = Instance.new("KeyframeSequence")
	P("ids ks", KSP:RegisterKeyframeSequence(sq) == KSP:RegisterKeyframeSequence(sq), KSP:RegisterKeyframeSequence(sq) == KSP:RegisterKeyframeSequence(Instance.new("KeyframeSequence")), ACP:RegisterAnimationClip(Instance.new("CurveAnimation")) == ACP:RegisterAnimationClip(Instance.new("CurveAnimation")))
end)
do
	local function c(setup) local a = Instance.new("CurveAnimation") local p = Instance.new("Vector3Curve") p.Name = "Position" p.Parent = folder("Arm", folder("Root", a)) fc("X", {FK(0, 0, L), FK(1, 10, L)}, p) if setup then setup(a) end return ACP:RegisterAnimationClip(a) end
	local base = c()
	P("id-content", "same build", base == c(), "name", base == c(function(a) a.Name = "Other" end), "loop", base == c(function(a) a.Loop = false end), "priority", base == c(function(a) a.Priority = Enum.AnimationPriority.Idle end), "extra child", base == c(function(a) folder("Unused", a) end), "attribute", base == c(function(a) a:SetAttribute("n", 1) end), "tag", base == c(function(a) a:AddTag("t") end), "joint name", base == c(function(a) a.Root.Arm.Name = "Arm2" end), "key", base == c(function(a) a.Root.Arm.Position:X():InsertKey(FK(0.5, 5, L)) end), "archivable", base == c(function(a) a.Archivable = false end))
	local f1, f2 = KSP:RegisterKeyframeSequence(Instance.new("Folder")), KSP:RegisterKeyframeSequence(Instance.new("Folder"))
	local g = Instance.new("Folder") g.Name = "G"
	P("id-nonclip", f1 == f2, f1 == KSP:RegisterKeyframeSequence(g), f1 == KSP:RegisterKeyframeSequence(Instance.new("Part")), f1 == KSP:RegisterKeyframeSequence(Instance.new("KeyframeSequence")), f1 == base)
	P("id-args", try(function() return KSP:RegisterKeyframeSequence() end), try(function() return KSP:RegisterKeyframeSequence(5) end), try(function() return KSP:RegisterKeyframeSequence("x") end), try(function() return ACP:RegisterAnimationClip() end), try(function() return ACP:RegisterAnimationClip(5) end))
end
return table.concat(out, "\n")
