-- `active://` ids: what an unknown one raises, and a track of AnimationClipProvider:RegisterActiveAnimationClip's id
-- following its CurveAnimation or KeyframeSequence as a script changes it (keys, joints, markers, Loop, Priority,
-- Length, the clip emptied and destroyed). An id's number counts what the session registered before, so it is
-- printed as N. Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode; active.txt is what Studio 0.747 returned (2026-11-11).
local out = {}
local function S(v) if type(v) == "number" then return string.format("%.9g", v) elseif type(v) == "table" then local t = {} for i, x in v do t[#t+1] = S(x) end local n = 0 for _ in v do n += 1 end return "{" .. table.concat(t, ", ") .. "}" .. (n ~= #v and "#" .. n or "") else return tostring(v) end end
local function P(...) local t = table.pack(...) for i = 1, t.n do t[i] = S(t[i]) end table.insert(out, (string.gsub(table.concat(t, " "), "active://%d+", "active://N"))) end
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
do
scene("unknown", function(animator, j1)
	local a = Instance.new("Animation") a.AnimationId = "active://99999"
	P("unknown track", try(function() return animator:LoadAnimation(a) end))
	a.AnimationId = "active://x" P("not a number", try(function() return animator:LoadAnimation(a) end))
	a.AnimationId = "active://-1" P("negative", try(function() return animator:LoadAnimation(a) end))
	a.AnimationId = "active://" P("empty", try(function() return animator:LoadAnimation(a) end))
	P("ACP get unknown", try(function() return ACP:GetAnimationClipAsync("active://99999") end))
	P("KSP get unknown", try(function() return KSP:GetKeyframeSequenceAsync("active://99999") end))
	local c = Instance.new("CurveAnimation") c.Loop = false
	posX(folder("Arm", folder("Root", c)), {FK(0, 0, L), FK(1, 10, L)})
	local id = ACP:RegisterActiveAnimationClip(c)
	local b = Instance.new("Animation") b.AnimationId = id
	local t2 = animator:LoadAnimation(b)
	t2.Looped = true t2.Priority = Enum.AnimationPriority.Idle
	c.Loop = false c.Priority = Enum.AnimationPriority.Movement
	P("set on track, then clip", tostring(t2.Looped), t2.Priority.Name)
	c.Loop = true
	P("clip Loop true", tostring(t2.Looped))
	t2.Looped = false c.Name = "Renamed"
	P("track set false, clip renamed", tostring(t2.Looped), tostring(c.Loop))
	t2:Play(0) animator:StepAnimations(0.5)
	c:FindFirstChild("Position", true):X():SetKeys({FK(0, 0, L), FK(0.25, 10, L)})
	P("shortened while at 0.5", string.format("L=%.6g t=%.6g", t2.Length, t2.TimePosition))
	animator:StepAnimations(0.1)
	P("after a step", string.format("L=%.6g t=%.6g x=%.6g %s", t2.Length, t2.TimePosition, j1.Transform.X, tostring(t2.IsPlaying)))
	local changed = 0
	t2:GetPropertyChangedSignal("Length"):Connect(function() changed += 1 end)
	c:FindFirstChild("Position", true):X():InsertKey(FK(3, 1, L)) task.wait()
	P("Length changed signal", changed, t2.Length)
end)
end
do
local function X(j) return string.format("%.6g", j.Transform.Position.X) end
scene("active curve", function(animator, j1, j2)
	local c = Instance.new("CurveAnimation") c.Loop = false
	local arm = folder("Arm", folder("Root", c))
	local pos = posX(arm, {FK(0, 0, L), FK(1, 10, L)})
	local mk = marker(c, "m", {{0.5, "half"}})
	local id = ACP:RegisterActiveAnimationClip(c)
	local a = Instance.new("Animation") a.AnimationId = id
	local t = animator:LoadAnimation(a)
	local read = watch(t, {"m", "n"})
	t:Play(0, 1, 0) animator:StepAnimations(0)
	local function show(label)
		local before = string.format("L=%.6g", t.Length)
		t.TimePosition = 0.5 animator:StepAnimations(0)
		P(label, id, before, string.format("L=%.6g %s %s arm=%s hand=%s", t.Length, tostring(t.Looped), t.Priority.Name, X(j1), X(j2)), read())
	end
	show("initial")
	pos:X():SetKeys({FK(0, 0, L), FK(1, 20, L)}) show("key value 20")
	pos:X():InsertKey(FK(2, 40, L)) show("key at 2 s")
	c.Loop = true show("clip Loop true")
	c.Priority = Enum.AnimationPriority.Core show("clip Priority Core")
	posX(folder("Hand", arm), {FK(0, 3, L), FK(2, 3, L)}) show("joint added")
	mk:InsertMarkerAtTime(0.25, "quarter") marker(c, "n", {{0.3, "new"}})
	t.TimePosition = 0.2 animator:StepAnimations(0) read() t:AdjustSpeed(1) animator:StepAnimations(0.4) P("markers after edit", string.format("%.6g", t.TimePosition), read()) t:AdjustSpeed(0)
	pos:X():SetKeys({FK(0, 100, L), FK(2, 100, L)}) P("mid-play edit before step", X(j1)) animator:StepAnimations(0) P("after step", X(j1))
	pos.Name = "Was" show("Position renamed")
	pos.Name = "Position" show("Position named back")
	arm.Name = "Hand" show("folder renamed to Hand")
	arm.Name = "Arm" show("folder named back")
	pos:Destroy() show("Position destroyed")
	local t2 = animator:LoadAnimation(a) P("second track", string.format("L=%.6g", t2.Length))
	c:ClearAllChildren() show("clip emptied")
	c:Destroy() show("clip destroyed")
	local ok, t3 = pcall(function() return animator:LoadAnimation(a) end) P("load after destroy", ok, ok and string.format("L=%.6g", t3.Length) or t3)
	P("get after destroy", try(function() return ACP:GetAnimationClipAsync(id) end))
end)
scene("active sequence", function(animator, j1, j2)
	local s = Instance.new("KeyframeSequence") s.Loop = false
	local function kf(time, x)
		local k = Instance.new("Keyframe") k.Time = time
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = k
		local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = CFrame.new(x, 0, 0) arm.Parent = root
		k.Parent = s
		return k, arm
	end
	local k0, p0 = kf(0, 0) local k1, p1 = kf(1, 10)
	local id = ACP:RegisterActiveAnimationClip(s)
	local a = Instance.new("Animation") a.AnimationId = id
	local t = animator:LoadAnimation(a)
	local read = watch(t, {"m"})
	t:Play(0, 1, 0) animator:StepAnimations(0)
	local function show(label)
		local before = string.format("L=%.6g", t.Length)
		t.TimePosition = 0.5 animator:StepAnimations(0)
		P(label, id, before, string.format("L=%.6g %s %s arm=%s", t.Length, tostring(t.Looped), t.Priority.Name, X(j1)), read())
	end
	show("initial")
	p1.CFrame = CFrame.new(20, 0, 0) show("pose value 20")
	local k2 = kf(2, 40) show("keyframe at 2 s")
	k1.Time = 1.5 show("keyframe moved to 1.5")
	s.Loop = true show("Loop true")
	s.Priority = Enum.AnimationPriority.Core show("Priority Core")
	k2:Destroy() show("last keyframe destroyed")
	local m = Instance.new("KeyframeMarker") m.Name = "m" m.Value = "v" m.Parent = k0
	k0.Name = "Start"
	t.TimePosition = 0 animator:StepAnimations(0) read() t:AdjustSpeed(1) animator:StepAnimations(0.1) P("marker and name after edit", read()) t:AdjustSpeed(0)
	s:Destroy() show("sequence destroyed")
end)
end
return table.concat(out, "\n")
