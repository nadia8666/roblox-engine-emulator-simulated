-- FaceControls: a CurveAnimation's FloatCurves in a Folder named FaceControls, and a KeyframeSequence's NumberPoses,
-- driving the properties of the rig's FaceControls instance: where the Folder and the instance may be, which names
-- count, clamping, blending by weight and priority, a track fading out, and the sequence made of such a clip.
-- Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode; face_controls.txt is what Studio 0.747 returned (2026-11-11).
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
local function V(x) return string.format("%.9g", x) end
scene("face", function(animator, j1, j2)
	local model = animator.Parent.Parent
	local arm = model.Arm
	local face = Instance.new("FaceControls") face.Parent = arm
	P("class", face.ClassName, face.Name, V(face.JawDrop))
	local c = Instance.new("CurveAnimation") c.Loop = false
	local armFolder = folder("Arm", folder("Root", c))
	posX(armFolder, {FK(0, 0, L), FK(1, 10, L)})
	local ff = folder("FaceControls", armFolder)
	fc("JawDrop", {FK(0, 0, L), FK(1, 1, L)}, ff)
	fc("EyesLookLeft", {FK(0, 2, L), FK(1, -1, L)}, ff)
	fc("NotAProperty", {FK(0, 0.5, L)}, ff)
	fc("Name", {FK(0, 0.5, L)}, ff)
	local t = load(animator, c)
	P("length", V(t.Length))
	t:Play(0, 1, 0) animator:StepAnimations(0)
	local row = {}
	for _, time in {0, 0.1, 0.25, 0.5, 0.75, 1} do
		t.TimePosition = time animator:StepAnimations(0)
		row[#row + 1] = string.format("%g: jaw=%s left=%s x=%s", time, V(face.JawDrop), V(face.EyesLookLeft), V(j1.Transform.X))
	end
	P("values", table.concat(row, "  "))
	face.JawDrop = 0.9 animator:StepAnimations(0) P("script write then step", V(face.JawDrop))
	face.Pucker = 0.7 animator:StepAnimations(0) P("unanimated property kept", V(face.Pucker))
	t:AdjustWeight(0.5, 0) animator:StepAnimations(0) t.TimePosition = 1 animator:StepAnimations(0) P("weight 0.5 at 1 s", V(face.JawDrop), V(face.EyesLookLeft), V(j1.Transform.X))
	t:Stop(0) animator:StepAnimations(0) P("stopped at once", V(face.JawDrop), V(face.Pucker))
	animator:StepAnimations(0.1) P("a step later", V(face.JawDrop), V(face.Pucker))
end)
end
do
local function V(x) return string.format("%.9g", x) end
local function faceClip(where, keys, loop, extra)
	local c = Instance.new("CurveAnimation") c.Loop = loop == true
	local root = folder("Root", c)
	local armFolder = folder("Arm", root)
	posX(armFolder, {FK(0, 0, L), FK(1, 10, L)})
	local parent = where == "arm" and armFolder or where == "root" and root or where == "clip" and c or where == "hand" and folder("Hand", armFolder) or folder(where, armFolder)
	local ff = folder(where == "other name" and "Face" or "FaceControls", parent)
	for name, list in keys do fc(name, list, ff) end
	if extra then extra(c, ff) end
	return c
end
local function ksClip(values)
	local s = Instance.new("KeyframeSequence") s.Loop = false
	for _, e in values do
		local k = Instance.new("Keyframe") k.Time = e[1]
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = k
		local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = CFrame.new(e[1] * 10, 0, 0) arm.Parent = root
		local ff = Instance.new("Folder") ff.Name = "FaceControls" ff.Parent = arm
		for name, value in e[2] do local n = Instance.new("NumberPose") n.Name = name n.Value = value n.Parent = ff end
		k.Parent = s
	end
	return s
end
local line = {FK(0, 0, L), FK(1, 1, L)}
scene("where", function(animator, j1, j2)
	local model = animator.Parent.Parent
	local face = Instance.new("FaceControls") face.Parent = model.Arm
	local function at(clip, time, read)
		local t = load(animator, clip) t:Play(0, 1, 0) animator:StepAnimations(0) t.TimePosition = time animator:StepAnimations(0)
		local r = read() t:Stop(0) animator:StepAnimations(0) face.JawDrop = 0 face.Pucker = 0
		return r
	end
	local jaw = function() return V(face.JawDrop) end
	P("folder under the joint", at(faceClip("arm", {JawDrop = line}), 0.5, jaw))
	P("folder under Root", at(faceClip("root", {JawDrop = line}), 0.5, jaw))
	P("folder under the clip", at(faceClip("clip", {JawDrop = line}), 0.5, jaw))
	P("folder under Hand", at(faceClip("hand", {JawDrop = line}), 0.5, jaw))
	P("folder under a Folder no joint has", at(faceClip("Nope", {JawDrop = line}), 0.5, jaw))
	P("folder of another name", at(faceClip("other name", {JawDrop = line}), 0.5, jaw))
	face.Parent = model.Hand P("face under Hand, folder under Arm", at(faceClip("arm", {JawDrop = line}), 0.5, jaw)) P("face under Hand, folder under Hand", at(faceClip("hand", {JawDrop = line}), 0.5, jaw))
	face.Parent = model P("face under the model", at(faceClip("arm", {JawDrop = line}), 0.5, jaw), at(faceClip("clip", {JawDrop = line}), 0.5, jaw))
	face.Parent = model.Arm
	local second = Instance.new("FaceControls") second.Name = "Second" second.Parent = model.Arm
	P("two under one part", at(faceClip("arm", {JawDrop = line}), 0.5, function() return V(face.JawDrop) .. "/" .. V(second.JawDrop) end))
	second.JawDrop = 0 second.Parent = model.Hand
	P("one under each part", at(faceClip("arm", {JawDrop = line}), 0.5, function() return V(face.JawDrop) .. "/" .. V(second.JawDrop) end))
	second:Destroy()
	face.Name = "Renamed" P("renamed instance", at(faceClip("arm", {JawDrop = line}), 0.5, jaw)) face.Name = "FaceControls"
	P("length of a face-only tail", V(load(animator, faceClip("arm", {JawDrop = {FK(0, 0, L), FK(3, 1, L)}})).Length))
	P("clamp", at(faceClip("arm", {JawDrop = {FK(0, 5, L), FK(1, 5, L)}, Pucker = {FK(0, -5, L), FK(1, -5, L)}}), 0.5, function() return V(face.JawDrop) .. "/" .. V(face.Pucker) end))
	P("cubic and constant", at(faceClip("arm", {JawDrop = {FK(0, 0, K), FK(1, 1, K)}, Pucker = {FK(0, 0.25, C), FK(0.5, 0.75, C), FK(1, 0.5, C)}}), 0.26, function() return V(face.JawDrop) .. "/" .. V(face.Pucker) end))
	local names = {}
	for _, name in {"ChinRaiser", "ChinRaiserUpperLip", "Corrugator", "CheekPuff", "EyesLookDown", "EyesLookLeft", "EyesLookRight", "EyesLookUp", "FlatPucker", "Funneler", "JawDrop", "JawLeft", "JawRight", "LeftBrowLowerer", "LeftCheekPuff", "LeftCheekRaiser", "LeftDimpler", "LeftEyeClosed", "LeftEyeUpperLidRaiser", "LeftInnerBrowRaiser", "LeftLipCornerDown", "LeftLipCornerPuller", "LeftLipStretcher", "LeftLowerLipDepressor", "LeftNoseWrinkler", "LeftOuterBrowRaiser", "LeftUpperLipRaiser", "LipPresser", "LipsTogether", "LowerLipSuck", "MouthLeft", "MouthRight", "Pucker", "RightBrowLowerer", "RightCheekPuff", "RightCheekRaiser", "RightDimpler", "RightEyeClosed", "RightEyeUpperLidRaiser", "RightInnerBrowRaiser", "RightLipCornerDown", "RightLipCornerPuller", "RightLipStretcher", "RightLowerLipDepressor", "RightNoseWrinkler", "RightOuterBrowRaiser", "RightUpperLipRaiser", "TongueDown", "TongueOut", "TongueUp", "UpperLipSuck", "jawDrop", "Jawdrop", "Name", "Archivable"} do names[name] = {FK(0, 0.5, L), FK(1, 0.5, L)} end
	P("every name", at(faceClip("arm", names), 0.5, function()
		local driven, not_driven = {}, {}
		for name in names do local ok, v = pcall(function() return face[name] end) if ok and v == 0.5 then driven[#driven + 1] = name else not_driven[#not_driven + 1] = name .. "=" .. tostring(ok and v) end end
		table.sort(driven) table.sort(not_driven)
		for name in names do pcall(function() if type(face[name]) == "number" then face[name] = 0 end end) end
		return #driven .. " driven; not: " .. table.concat(not_driven, " ")
	end))
end)
scene("blend", function(animator, j1, j2)
	local model = animator.Parent.Parent
	local face = Instance.new("FaceControls") face.Parent = model.Arm
	local function clipOf(v, priority) local c = faceClip("arm", {JawDrop = {FK(0, v, L), FK(1, v, L)}}, true) c.Priority = priority or Enum.AnimationPriority.Action return c end
	local a = load(animator, clipOf(0.8)) local b = load(animator, clipOf(0.2)) local hi = load(animator, clipOf(0.4, Enum.AnimationPriority.Action2))
	local row = {}
	local function show(label) animator:StepAnimations(0) row[#row + 1] = label .. "=" .. V(face.JawDrop) end
	a:Play(0) show("a") b:Play(0) show("a+b") b:AdjustWeight(3, 0) show("b weight 3") b:AdjustWeight(0.25, 0) a:AdjustWeight(0.25, 0) show("both 0.25")
	hi:Play(0, 0.5) show("higher at 0.5") hi:AdjustWeight(1, 0) show("higher at 1") hi:AdjustWeight(0.0005, 0) show("higher below 0.001")
	a:Stop(0) b:Stop(0) hi:Stop(0) animator:StepAnimations(0)
	face.JawDrop = 0.3
	a:Play(0, 0.5) show("half over a written 0.3")
	a:AdjustWeight(0.0005, 0) show("weight below 0.001")
	a:Stop(0.2) animator:StepAnimations(0.1) row[#row + 1] = "fading=" .. V(face.JawDrop) animator:StepAnimations(0.1) row[#row + 1] = "faded=" .. V(face.JawDrop) animator:StepAnimations(0.1) row[#row + 1] = "ended=" .. V(face.JawDrop)
	P("blend", table.concat(row, "  "))
	local other = load(animator, faceClip("arm", {Pucker = {FK(0, 0.6, L), FK(1, 0.6, L)}}, true))
	face.JawDrop = 0.3 other:Play(0) animator:StepAnimations(0)
	P("a property no playing track has, while another plays", V(face.JawDrop), V(face.Pucker))
	other:Stop(0) animator:StepAnimations(0)
	local plain = Instance.new("CurveAnimation") posX(folder("Arm", folder("Root", plain)), {FK(0, 0, L), FK(1, 10, L)})
	local body = load(animator, plain) face.JawDrop = 0.3 face.Pucker = 0.3 body:Play(0) animator:StepAnimations(0)
	P("while a track without face curves plays", V(face.JawDrop), V(face.Pucker))
	body:Stop(0) animator:StepAnimations(0)
end)
scene("sequence", function(animator, j1, j2)
	local model = animator.Parent.Parent
	local face = Instance.new("FaceControls") face.Parent = model.Arm
	local t = load(animator, ksClip({{0, {JawDrop = 0, Pucker = 2}}, {1, {JawDrop = 1, Pucker = 2}}}))
	t:Play(0, 1, 0) animator:StepAnimations(0)
	local row = {}
	for _, time in {0, 0.1, 0.5, 1} do t.TimePosition = time animator:StepAnimations(0) row[#row + 1] = string.format("%g: jaw=%s pucker=%s x=%s", time, V(face.JawDrop), V(face.Pucker), V(j1.Transform.X)) end
	P("sequence face poses", table.concat(row, "  "))
	local made = KSP:GetKeyframeSequenceAsync(select(2, load(animator, faceClip("arm", {JawDrop = line}))))
	local kinds = {}
	for _, d in made:GetChildren()[2]:GetDescendants() do kinds[#kinds + 1] = d.ClassName .. ":" .. d.Name .. (d:IsA("NumberPose") and "=" .. V(d.Value) or "") end
	P("sequence made of a curve clip", #made:GetChildren(), table.concat(kinds, " "))
end)
end
scene("face fade", function(animator, j1)
	local function V(x) return string.format("%.9g", x) end
	local face = Instance.new("FaceControls") face.Parent = animator.Parent.Parent.Arm
	local c = Instance.new("CurveAnimation") c.Loop = true
	local armFolder = folder("Arm", folder("Root", c))
	posX(armFolder, {FK(0, 0, L), FK(1, 0, L)})
	fc("JawDrop", {FK(0, 0.9, L), FK(1, 0.9, L)}, folder("FaceControls", armFolder))
	local f = load(animator, c)
	local row = {}
	row = {}
	f:Play(0) animator:StepAnimations(0) f:Stop(0.3)
	for i = 1, 5 do animator:StepAnimations(0.1) row[#row + 1] = string.format("w=%s jaw=%s", V(f.WeightCurrent), V(face.JawDrop)) end
	P("face fading out", table.concat(row, "  "))
end)
return table.concat(out, "\n")
