-- A track's weight over its fades, in float32: Play, Stop and AdjustWeight with and without a fade time, a fade
-- changed half way, a negative step, what Stopped and Ended fire and when, a track ended at once, a destroyed
-- track, and which joints a step writes. For a CurveAnimation and a KeyframeSequence. Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode;
-- playback_weights.txt is what Studio 0.747 returned (2026-11-11).
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
local function X(j) return string.format("%.9g", j.Transform.Position.X) end
local function clipOf()
	local c = Instance.new("CurveAnimation") c.Loop = true
	posX(folder("Arm", folder("Root", c)), {FK(0, 10, L), FK(1, 10, L)})
	return c
end
local function run(label, start, steps)
	scene(label, function(animator, j1)
		local t = load(animator, clipOf())
		start(t, animator)
		local row = {}
		for _, d in steps do animator:StepAnimations(d) row[#row + 1] = string.format("%.9g/%s", t.WeightCurrent, X(j1)) end
		P(label, table.concat(row, " "))
	end)
end
local function rep(d, n) local t = {} for i = 1, n do t[i] = d end return t end
run("A play .3 by .05", function(t) t:Play(0.3) end, rep(0.05, 8))
run("B stop .3 by .05", function(t, a) t:Play(0) a:StepAnimations(0) t:Stop(0.3) end, rep(0.05, 8))
run("C play .7 by .1", function(t) t:Play(0.7) end, rep(0.1, 9))
run("D adjust .5/.25 by .03", function(t, a) t:Play(0) a:StepAnimations(0) t:AdjustWeight(0.5, 0.25) end, rep(0.03, 10))
run("E play .3 mixed", function(t) t:Play(0.3) end, {0.01, 0.07, 0.033, 0.1, 0.002, 0.05, 0.05})
run("F play 1 w .6 by 1/60", function(t) t:Play(1, 0.6) end, rep(1 / 60, 12))
run("G stop .1 by 1/60", function(t, a) t:Play(0) a:StepAnimations(0) t:Stop() end, rep(1 / 60, 8))
run("H play .3 then stop .3 midway", function(t, a) t:Play(0.3) a:StepAnimations(0.1) t:Stop(0.3) end, rep(0.05, 8))
run("I play .3, replay", function(t, a) t:Play(0.3) a:StepAnimations(0.1) t:Play(0.2, 2) end, rep(0.05, 6))
end
do
local function X(j) return string.format("%.9g", j.Transform.Position.X) end
local function clipOf()
	local c = Instance.new("CurveAnimation") c.Loop = true
	posX(folder("Arm", folder("Root", c)), {FK(0, 1000, L), FK(1, 1000, L)})
	return c
end
local function run(label, start, steps)
	scene(label, function(animator, j1)
		local t = load(animator, clipOf())
		local row = {}
		local function show() row[#row + 1] = string.format("%.9g/%s/%s", t.WeightCurrent, X(j1), tostring(t.IsPlaying)) end
		start(t, animator, show)
		for _, d in steps do animator:StepAnimations(d) show() end
		P(label, table.concat(row, " "))
	end)
end
run("a adjust .5,0", function(t, a, show) t:Play(0) a:StepAnimations(0) show() t:AdjustWeight(0.5, 0) show() end, {0, 0.1, 0.1})
run("b adjust .5", function(t, a, show) t:Play(0) a:StepAnimations(0) t:AdjustWeight(0.5) show() end, {0.05, 0.05, 0.05})
run("c stop 0", function(t, a, show) t:Play(0) a:StepAnimations(0) t:Stop(0) show() end, {0, 0.1, 0.1})
run("d play 0", function(t, a, show) t:Play(0) show() end, {0, 0.1})
run("e play 0 w .25", function(t, a, show) t:Play(0, 0.25) show() end, {0, 0.1})
run("f play .1 step 0", function(t, a, show) t:Play(0.1) show() end, {0, 0, 0.05, 0, 0.05, 0})
for _, w in {1e-8, 3.1e-8, 6e-8, 1.2e-7, 1e-6, 1e-5, 1e-4, 1e-3} do
	run("g tiny " .. w, function(t, a, show) t:Play(0) a:StepAnimations(0) t:AdjustWeight(w, 0.001) end, {0.01, 0.01})
end
run("h adjust negative", function(t, a, show) t:Play(0) a:StepAnimations(0) t:AdjustWeight(-1, 0.001) end, {0.01, 0.01})
run("i adjust .5,-1", function(t, a, show) t:Play(0) a:StepAnimations(0) t:AdjustWeight(0.5, -1) show() end, {0, 0.1})
run("j play -1 fade", function(t, a, show) t:Play(-1) show() end, {0, 0.1})
end
do
local function X(j) return string.format("%.9g", j.Transform.Position.X) end
local function clipOf(x, loop, ks)
	if ks then
		local s = Instance.new("KeyframeSequence") s.Loop = loop ~= false
		for _, time in {0, 1} do
			local kf = Instance.new("Keyframe") kf.Time = time
			local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
			local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = CFrame.new(x or 1000, 0, 0) arm.Parent = root
			kf.Parent = s
		end
		return s
	end
	local c = Instance.new("CurveAnimation") c.Loop = loop ~= false
	posX(folder("Arm", folder("Root", c)), {FK(0, x or 1000, L), FK(1, x or 1000, L)})
	return c
end
local function run(label, ks, start, steps)
	scene(label, function(animator, j1)
		local t = load(animator, clipOf(nil, nil, ks))
		local row, events = {}, {}
		t.Stopped:Connect(function() events[#events + 1] = "Stopped" end)
		t.Ended:Connect(function() events[#events + 1] = "Ended" end)
		local function show() task.wait() row[#row + 1] = string.format("%.9g/%s/%s%s", t.WeightCurrent, X(j1), t.IsPlaying and "p" or "-", #events > 0 and "<" .. table.concat(events, ",") .. ">" or "") table.clear(events) end
		start(t, animator, show, j1)
		for _, d in steps do animator:StepAnimations(d) show() end
		P(label, table.concat(row, " "))
	end)
end
for _, ks in {false, true} do
	local k = ks and "KS " or "CA "
	run(k .. "stop 0", ks, function(t, a, show) t:Play(0) a:StepAnimations(0) t:Stop(0) show() end, {0, 0.1, 0.5})
	run(k .. "stop 0 replay", ks, function(t, a, show) t:Play(0) a:StepAnimations(0) t:Stop(0) a:StepAnimations(0.1) show() t:Play(0.2) show() end, {0.1, 0.1, 0.1})
	run(k .. "stop .1", ks, function(t, a, show) t:Play(0) a:StepAnimations(0) t:Stop(0.1) show() end, {0.05, 0.05, 0.05, 0.05})
	run(k .. "adjust 0,0", ks, function(t, a, show) t:Play(0) a:StepAnimations(0) t:AdjustWeight(0, 0) show() end, {0, 0.1, 0.1})
	run(k .. "adjust 0,.1", ks, function(t, a, show) t:Play(0) a:StepAnimations(0) t:AdjustWeight(0, 0.1) show() end, {0.05, 0.05, 0.05, 0.05})
	run(k .. "destroy", ks, function(t, a, show, j) t:Play(0) a:StepAnimations(0) show() t:Destroy() row = nil end, {})
	run(k .. "play 100", ks, function(t, a, show) t:Play(100) show() end, {0.0001, 0.0001, 0.001, 0.01, 0.1})
	for _, w in {2e-5, 5e-5, 9e-5, 2e-4, 5e-4, 9e-4, 9.9e-4, 1.01e-3} do
		run(k .. "tiny " .. w, ks, function(t, a, show) t:Play(0) a:StepAnimations(0) t:AdjustWeight(w, 0.001) end, {0.01, 0.01})
	end
end
end
do
local function X(j) return string.format("%.9g", j.Transform.Position.X) end
local function clipOf(name, x, ks)
	if ks then
		local s = Instance.new("KeyframeSequence") s.Loop = true
		for _, time in {0, 1} do
			local kf = Instance.new("Keyframe") kf.Time = time
			local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
			local arm = Instance.new("Pose") arm.Name = "Arm" arm.Weight = name == "Arm" and 1 or 0 arm.CFrame = CFrame.new(name == "Arm" and x or 0, 0, 0) arm.Parent = root
			if name == "Hand" then local hand = Instance.new("Pose") hand.Name = "Hand" hand.CFrame = CFrame.new(x, 0, 0) hand.Parent = arm end
			kf.Parent = s
		end
		return s
	end
	local c = Instance.new("CurveAnimation") c.Loop = true
	posX(folder(name, folder("Root", c)), {FK(0, x, L), FK(1, x, L)})
	return c
end
for _, ks in {false, true} do
	local k = ks and "KS " or "CA "
	scene(k, function(animator, j1, j2)
		local a = load(animator, clipOf("Arm", 1000, ks))
		local b = load(animator, clipOf("Hand", 300, ks))
		local c = load(animator, clipOf("Arm", 500, ks))
		local row = {}
		local function show(label) row[#row + 1] = string.format("%s:%s/%s[%.9g %.9g|%.9g %.9g]", label, X(j1), X(j2), a.WeightCurrent, a.WeightTarget, c.WeightCurrent, c.WeightTarget) end
		a:Play(0) b:Play(0) animator:StepAnimations(0) show("both")
		a:Stop(0) show("a stop0") animator:StepAnimations(0.1) show("step") animator:StepAnimations(0.1) show("step")
		c:Play(0, 1e-4) animator:StepAnimations(0) show("c tiny") c:Stop(0) animator:StepAnimations(0.1) show("c stop0")
		a:Play(0) c:Play(0, 1) animator:StepAnimations(0) show("a+c") a:Stop(0) animator:StepAnimations(0) show("a stop0") c:Stop(0) animator:StepAnimations(0) show("c stop0")
		c:Play() show("c play") animator:StepAnimations(0.05) show("step") c:AdjustWeight(0, 0) animator:StepAnimations(0) show("c adjust00") c:Play() show("c play") animator:StepAnimations(0.05) show("step")
		j1.Transform = CFrame.new(7, 0, 0) animator:StepAnimations(0.05) show("set 7, step")
		c:Stop(0) animator:StepAnimations(0) j1.Transform = CFrame.new(7, 0, 0) animator:StepAnimations(0.05) show("stale set 7, step")
		a:Play(0) animator:StepAnimations(0) show("a") a:Destroy() animator:StepAnimations(0) show("a destroyed") animator:StepAnimations(0.1) show("step")
		b:Stop(0.1) animator:StepAnimations(0.05) show("b stop.1") animator:StepAnimations(0.05) show("step") animator:StepAnimations(0.05) show("step")
		P(k, table.concat(row, "  "))
	end)
end
end
do
local function X(j) return string.format("%.9g", j.Transform.Position.X) end
local function clipOf(name, x, ks)
	if ks then
		local s = Instance.new("KeyframeSequence") s.Loop = true
		for _, time in {0, 1} do
			local kf = Instance.new("Keyframe") kf.Time = time
			local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = kf
			local arm = Instance.new("Pose") arm.Name = "Arm" arm.Weight = name == "Arm" and 1 or 0 arm.CFrame = CFrame.new(name == "Arm" and x or 0, 0, 0) arm.Parent = root
			if name == "Hand" then local hand = Instance.new("Pose") hand.Name = "Hand" hand.CFrame = CFrame.new(x, 0, 0) hand.Parent = arm end
			kf.Parent = s
		end
		return s
	end
	local c = Instance.new("CurveAnimation") c.Loop = true
	posX(folder(name, folder("Root", c)), {FK(0, x, L), FK(1, x, L)})
	return c
end
for _, ks in {false, true} do
	local k = ks and "KS " or "CA "
	scene(k, function(animator, j1, j2, bone)
		local a = load(animator, clipOf("Arm", 1000, ks))
		local row = {}
		local function show(label) row[#row + 1] = string.format("%s:%s/%s/%s", label, X(j1), X(j2), X(bone)) end
		local function seven() j1.Transform = CFrame.new(7, 0, 0) j2.Transform = CFrame.new(7, 0, 0) bone.Transform = CFrame.new(7, 0, 0) end
		seven() animator:StepAnimations(0.1) show("no track")
		a:Play(0) animator:StepAnimations(0) show("a only")
		local b = load(animator, clipOf("Hand", 300, ks))
		seven() animator:StepAnimations(0.1) show("b loaded")
		b:Play(0) animator:StepAnimations(0) show("b played") b:Stop(0) animator:StepAnimations(0) show("b stop0") seven() animator:StepAnimations(0.1) show("seven, step")
		b:Play(0) animator:StepAnimations(0) b:Stop(0.1) animator:StepAnimations(0.2) animator:StepAnimations(0.2) show("b faded") seven() animator:StepAnimations(0.1) show("seven, step")
		a:Stop(0.1) animator:StepAnimations(0.05) seven() animator:StepAnimations(0.05) show("a fading, seven, step") seven() animator:StepAnimations(0.05) show("ended, seven, step") seven() animator:StepAnimations(0.05) show("seven, step")
		P(k, table.concat(row, "  "))
	end)
end
end
return table.concat(out, "\n")
