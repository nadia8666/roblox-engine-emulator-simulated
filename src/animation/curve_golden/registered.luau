-- What GetKeyframeSequenceAsync and GetAnimationClipAsync give for a registered id: the clip as it was registered,
-- the same instance every call, also after a script changed or destroyed it, and what a clip that is not Archivable
-- registers as. Runs unchanged in Studio (the command bar or a plugin, in an edit session) and here in edit mode; registered.txt is what Studio 0.747 returned (2026-11-11)
-- in a session that had not asked for such an id before: the first request for the id of what is no clip raises, later
-- ones do not.
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
local SEED = (os.clock() * 1000) % 1000 + 1
local function ksOf()
	local s = Instance.new("KeyframeSequence") s.Loop = true s.Name = "Seq" s.Priority = Enum.AnimationPriority.Idle
	for _, e in {{0, 0}, {1, SEED}} do
		local k = Instance.new("Keyframe") k.Time = e[1] k.Name = "K" .. e[1] k:SetAttribute("A", 1)
		local root = Instance.new("Pose") root.Name = "Root" root.Weight = 0 root.Parent = k
		local arm = Instance.new("Pose") arm.Name = "Arm" arm.CFrame = CFrame.new(e[2], 0, 0) arm.EasingStyle = Enum.PoseEasingStyle.Cubic arm.EasingDirection = Enum.PoseEasingDirection.Out arm.Parent = root
		local m = Instance.new("KeyframeMarker") m.Name = "M" m.Value = "v" .. e[1] m.Parent = k
		local f = Instance.new("Folder") f.Name = "Stray" f.Parent = k
		k.Parent = s
	end
	local stray = Instance.new("StringValue") stray.Name = "Note" stray.Parent = s
	return s
end
local function shape(v)
	if typeof(v) ~= "Instance" then return tostring(v) end
	local parts = {v.ClassName .. ":" .. v.Name .. (v:IsA("KeyframeSequence") and (":" .. tostring(v.Loop) .. ":" .. v.Priority.Name) or "")}
	for _, d in v:GetDescendants() do
		local extra = ""
		if d:IsA("Keyframe") then extra = "@" .. S(d.Time) .. "#" .. tostring(d:GetAttribute("A")) elseif d:IsA("Pose") then extra = "=" .. S(math.round(d.CFrame.X / SEED * 10)) .. "/" .. S(d.Weight) .. "/" .. d.EasingStyle.Name .. "/" .. d.EasingDirection.Name elseif d:IsA("KeyframeMarker") then extra = "=" .. d.Value end
		parts[#parts + 1] = d.ClassName .. ":" .. d.Name .. extra
	end
	return table.concat(parts, " ")
end
local s = ksOf() s:SetAttribute("Kept", 1) s:AddTag("T")
local sid = KSP:RegisterKeyframeSequence(s)
local a = KSP:GetKeyframeSequenceAsync(sid)
local b = KSP:GetKeyframeSequenceAsync(sid)
P("KSP copy", shape(a))
P("KSP twice same", a == b, a == s, tostring(a.Parent), a:GetAttribute("Kept"), a:HasTag("T"))
a.Name = "Touched" Instance.new("Folder", a)
local c = KSP:GetKeyframeSequenceAsync(sid)
P("KSP after touching the copy", c == a, c.Name, #c:GetChildren())
a:Destroy()
local d = KSP:GetKeyframeSequenceAsync(sid)
P("KSP after destroying the copy", d == a, d.Name, #d:GetChildren())
local e = ACP:GetAnimationClipAsync(sid)
local f = ACP:GetAnimationClipAsync(sid)
P("ACP copy", shape(e))
P("ACP twice same", e == f, e == s, e == d, tostring(e.Parent), e:GetAttribute("Kept"), e:HasTag("T"))
local cv = Instance.new("CurveAnimation") cv.Name = "Clip" cv:SetAttribute("Kept", 1)
posX(folder("Arm", folder("Root", cv)), {FK(0, 0, L), FK(1, SEED, L)})
local cid = ACP:RegisterAnimationClip(cv)
local g = ACP:GetAnimationClipAsync(cid) local h = ACP:GetAnimationClipAsync(cid)
P("ACP curve twice same", g == h, g == cv, g.Name, g:GetAttribute("Kept"))
local i = KSP:GetKeyframeSequenceAsync(cid) local j = KSP:GetKeyframeSequenceAsync(cid)
P("KSP curve twice same", i == j, i.Name)
local function timed(label, f)
	local done = false
	task.spawn(function() local r = try(f) done = true P(label, r) end)
	local t = os.clock()
	while not done and os.clock() - t < 4 do task.wait() end
	if not done then P(label, "NEVER RETURNED") end
end
local n = ksOf() n.Archivable = false
local nid = KSP:RegisterKeyframeSequence(n)
timed("not archivable KSP", function() return shape(KSP:GetKeyframeSequenceAsync(nid)) end)
timed("not archivable KSP again", function() return shape(KSP:GetKeyframeSequenceAsync(nid)) end)
timed("not archivable ACP", function() return shape(ACP:GetAnimationClipAsync(nid)) end)
timed("not archivable KSP after ACP", function() return shape(KSP:GetKeyframeSequenceAsync(nid)) end)
local nc = Instance.new("CurveAnimation") nc.Archivable = false posX(folder("Arm", folder("Root", nc)), {FK(0, 0, L), FK(1, SEED + 1, L)})
local ncid = ACP:RegisterAnimationClip(nc)
P("ids of what is no clip", nid == ncid, nid == KSP:RegisterKeyframeSequence(Instance.new("Part")), nid == KSP:RegisterKeyframeSequence(Instance.new("KeyframeSequence")))
timed("curve not archivable KSP", function() return #KSP:GetKeyframeSequenceAsync(ncid):GetChildren() end)
timed("curve not archivable ACP", function() return shape(ACP:GetAnimationClipAsync(ncid)) end)
scene("track of not archivable", function(animator, j1)
	local a = Instance.new("Animation") a.AnimationId = nid
	local t = animator:LoadAnimation(a) for i = 1, 5 do task.wait() end
	local b = Instance.new("Animation") b.AnimationId = ncid
	local t2 = animator:LoadAnimation(b) for i = 1, 5 do task.wait() end
	P("tracks of not archivable", S(t.Length), S(t2.Length))
end)
return table.concat(out, "\n")
