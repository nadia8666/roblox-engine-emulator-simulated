-- Tracks of one joint blended by priority and weight: thirteen mixes of five tracks on four priorities, for
-- CurveAnimations and for KeyframeSequences, and a track that was stopped, adjusted and ended. Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode;
-- playback_blend.txt is what Studio 0.747 returned (2026-11-11).
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
local function V(x) return string.format("%.9g", x) end
local function clipOf(x, priority, ks)
	if ks then
		local s = Instance.new("KeyframeSequence") s.Loop = true s.Priority = priority
		for _, time in {0, 1} do
			local k = Instance.new("Keyframe") k.Time = time
			local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = k
			local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = CFrame.new(x, 0, 0) arm.Parent = root
			k.Parent = s
		end
		return s
	end
	local c = Instance.new("CurveAnimation") c.Loop = true c.Priority = priority
	local armFolder = folder("Arm", folder("Root", c))
	posX(armFolder, {FK(0, x, L), FK(1, x, L)})
	return c
end
local A, A2, A3, M = Enum.AnimationPriority.Action, Enum.AnimationPriority.Action2, Enum.AnimationPriority.Action3, Enum.AnimationPriority.Movement
for _, ks in {false, true} do
	scene(ks and "KS" or "CA", function(animator, j1)
		local a, b, hi, top, low = load(animator, clipOf(8, A, ks)), load(animator, clipOf(2, A, ks)), load(animator, clipOf(4, A2, ks)), load(animator, clipOf(16, A3, ks)), load(animator, clipOf(32, M, ks))
		local row = {}
		local function show(label) animator:StepAnimations(0) row[#row + 1] = label .. "=" .. V(j1.Transform.X) end
		local function weights(list) for track, w in list do if w == false then track:Stop(0) else if not track.IsPlaying then track:Play(0, w) else track:AdjustWeight(w, 0) end end end end
		weights({[a] = 0.25, [b] = 0.25}) show("a .25, b .25")
		weights({[hi] = 0.5}) show("+ higher .5")
		weights({[hi] = 0.25}) show("higher .25")
		weights({[hi] = 0.9}) show("higher .9")
		weights({[a] = 1, [b] = 1, [hi] = 0.5}) show("a 1, b 1, higher .5")
		weights({[a] = 3, [b] = 1, [hi] = 0.5}) show("a 3, b 1, higher .5")
		weights({[a] = 0.1, [b] = 0.1, [hi] = 0.5, [low] = 0.2}) show("a .1, b .1, higher .5, lower .2")
		weights({[low] = 1}) show("lower 1")
		weights({[top] = 0.25}) show("top .25")
		weights({[top] = 2}) show("top 2")
		weights({[top] = false, [hi] = false, [low] = false, [a] = 0.4, [b] = 0.8}) show("a .4, b .8")
		weights({[a] = 0.4, [b] = 0.2}) show("a .4, b .2")
		weights({[a] = -1, [b] = 0.5}) show("a -1, b .5")
		P(ks and "KS" or "CA", table.concat(row, "  "))
	end)
end
scene("stopped track", function(animator, j1)
	local t = load(animator, clipOf(8, A, false))
	local row = {}
	t:Play(0) animator:StepAnimations(0) t:Stop(0.3) t:AdjustWeight(0.5, 0) animator:StepAnimations(0) row[#row + 1] = string.format("after Stop(.3), AdjustWeight(.5, 0), step 0: w=%s target=%s x=%s", V(t.WeightCurrent), V(t.WeightTarget), V(j1.Transform.X))
	animator:StepAnimations(0.1) row[#row + 1] = string.format("step .1: w=%s x=%s", V(t.WeightCurrent), V(j1.Transform.X))
	t:AdjustSpeed(2) t.TimePosition = 0.25 animator:StepAnimations(0.1) row[#row + 1] = string.format("speed 2 while stopped: t=%s w=%s", V(t.TimePosition), V(t.WeightCurrent))
	animator:StepAnimations(0.2) animator:StepAnimations(0.1) row[#row + 1] = string.format("ended: w=%s x=%s", V(t.WeightCurrent), V(j1.Transform.X))
	P("stopped track", table.concat(row, "  "))
end)
return table.concat(out, "\n")
