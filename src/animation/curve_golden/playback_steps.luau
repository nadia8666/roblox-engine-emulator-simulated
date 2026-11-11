-- A CurveAnimation's track stepped with Animator:StepAnimations: its time, what it fires and when it ends, forwards
-- and backwards, looped and not, landing on markers and on the clip's ends, with TimePosition set, and stopped. Runs
-- unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode; playback_steps.txt is
-- what Studio 0.747 returned (2026-11-11). Poses are printed to four decimals.
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
for _, loop in {false, true} do
	local l = if loop then "loop" else "once"
	run("boundary-" .. l, clip(loop, {{0.25, "q"}, {0.5, "h"}}), 1, {0.25, 0.25, 0.25, 0.25, 0.25})
	run("boundary-back-" .. l, clip(loop, {{0.25, "q"}, {0.5, "h"}}), -1, {0.25, 0.25, 0.25, 0.25, 0.25})
	run("ends-" .. l, clip(loop, {{0, "zero"}, {1, "one"}}), 1, {0.75, 0.5, 0.75, 0.25})
	run("ends-back-" .. l, clip(loop, {{0, "zero"}, {1, "one"}}), -1, {0.25, 0.5, 0.5, 0.5})
	run("exact-zero-back-" .. l, clip(loop, {{0.25, "q"}}), -1, {0.5, 0.5, 0.25, 0.25})
	run("set-" .. l, clip(loop, {{0.25, "q"}}), 1, {{5, 0}, {-1, 0}, {1, 0}, {0.25, 0}, 0.125, {0.25, 0.125}, {0.9, 0.05}})
	run("set-back-" .. l, clip(loop, {{0.25, "q"}}), -1, {{0, 0}, {0.5, 0}, 0.125})
end
local function cline(loop) local c = Instance.new("CurveAnimation") c.Loop = loop posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(1, 10, L)}) marker(c, "M", {{0.25, "a"}, {0.75, "b"}}) return c end
for _, kind in {"curve"} do
	for _, loop in {false, true} do
		local function make() return cline(loop) end
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
		end)
	end
end
-- a marker at 0: when it fires
for _, kind in {"curve"} do
	scene("start", function(animator, j1)
		local clip
		clip = Instance.new("CurveAnimation") clip.Loop = false posX(folder("Arm", folder("Root", clip)), {FK(0, 0, L), FK(1, 10, L)}) marker(clip, "M", {{0, "zero"}, {1, "one"}})
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
scene("step-args", function(animator, j1)
	P("step-args", try(animator.StepAnimations, animator), try(animator.StepAnimations, animator, "x"), try(animator.StepAnimations, animator, "0.5"), try(animator.StepAnimations, animator, {}))
	local t = load(animator, cline(true)) t:Play(0, 1, 1) animator:StepAnimations(0.25)
	local other = Instance.new("Animator")
	P("step-other", try(other.StepAnimations, other, 0.25), S(t.TimePosition))
	animator:StepAnimations("0.25") P("step-string", S(t.TimePosition))
end)
return table.concat(out, "\n")
