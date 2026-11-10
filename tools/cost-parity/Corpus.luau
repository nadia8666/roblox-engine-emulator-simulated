-- The parity bench's workloads (tools/cost-parity): one source that runs unchanged in ree and in the real engine, so the
-- two clocks read the same work. In the engine os.clock is the real cost; in ree it is the work clock (src/cost.rs).
-- parity.py makes it a ModuleScript (with Language.luau twice: interpreted and `--!native`) inside a job or a harness
-- and calls Run(Options). A workload runs `Rounds` rounds of `N` operations, each round in a frame of its own, and
-- reports nanoseconds an operation (the median round, the loop included); a `mem` workload reports bytes an item
-- (collectgarbage("count") across building what it keeps). No workload reads or draws anything a headless engine
-- lacks.
local Players = game:GetService("Players")
local ReplicatedStorage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
local CollectionService = game:GetService("CollectionService")
local TweenService = game:GetService("TweenService")
local HttpService = game:GetService("HttpService")

local Corpus = {}
local List = {}
-- Every round adds a million to it: a workload that makes strings from numbers makes new ones in every round (a changing figure in a
-- UI), not the ones the round before left alive (strings are interned)
local Fresh = 0

-- Family, name, N (operations a round), Fn(N, S, State) -> nil or the seconds it measured itself; Options.Prepare(N, S)
-- -> State runs before each round and Options.Cleanup(State, S) after it, both outside the timed span.
local function W(Family, Name, N, Fn, Options)
	Options = Options or {}
	table.insert(List, { Family = Family, Name = Family .. "." .. Name, N = N, Fn = Fn, Prepare = Options.Prepare, Cleanup = Options.Cleanup, Finish = Options.Finish, Measured = Options.Measured, Memory = Options.Memory })
end

local function median(list)
	local sorted = table.clone(list)
	table.sort(sorted)
	return sorted[(#sorted + 1) // 2]
end

------------------------------------------------------------------------------------------------ the scene

local function part(Parent, Name)
	local P = Instance.new("Part")
	P.Name = Name or "Part"
	P.Anchored = true
	P.Size = Vector3.new(2, 2, 2)
	P.CFrame = CFrame.new(0, 200, 0)
	P.Parent = Parent
	return P
end

local function tree(Parent, Name, Width, Depth, Leaf)
	local Root = Instance.new("Folder")
	Root.Name = Name
	local function fill(Node, Level)
		for I = 1, Width do
			local Child
			if Level == Depth then
				Child = Instance.new(Leaf or "Folder")
			else
				Child = Instance.new("Folder")
				fill(Child, Level + 1)
			end
			Child.Name = "N" .. Level .. "_" .. I
			Child.Parent = Node
		end
	end
	fill(Root, 1)
	Root.Parent = Parent
	return Root
end

local function guiTree(Parent)
	local Frame = Instance.new("Frame")
	Frame.Name = "Card"
	Frame.Size = UDim2.fromOffset(160, 90)
	local Label = Instance.new("TextLabel")
	Label.Text = "12"
	Label.Size = UDim2.fromScale(1, 0.5)
	Label.Parent = Frame
	local Image = Instance.new("ImageLabel")
	Image.Size = UDim2.fromScale(0.5, 0.5)
	Image.Parent = Frame
	Instance.new("UICorner").Parent = Frame
	Instance.new("UIStroke").Parent = Frame
	Instance.new("UIGradient").Parent = Frame
	for I = 1, 2 do
		local Child = Instance.new("Frame")
		Child.Name = "Pip" .. I
		Child.Size = UDim2.fromOffset(8, 8)
		Child.Parent = Frame
	end
	Frame.Parent = Parent
	return Frame
end

local function effectPart(Parent)
	local P = part(Parent, "Effect")
	local A0 = Instance.new("Attachment")
	A0.Parent = P
	local A1 = Instance.new("Attachment")
	A1.Position = Vector3.new(0, 2, 0)
	A1.Parent = P
	for I = 1, 3 do
		local E = Instance.new("ParticleEmitter")
		E.Name = "Emitter" .. I
		E.Enabled = false
		E.Parent = A0
	end
	local B = Instance.new("Beam")
	B.Attachment0 = A0
	B.Attachment1 = A1
	B.Parent = P
	return P
end

local function model(Parent, Count)
	local M = Instance.new("Model")
	M.Name = "Model" .. Count
	for I = 1, Count do
		local P = part(M, "P" .. I)
		P.CFrame = CFrame.new(I * 3, 200, 0)
		if I == 1 then
			M.PrimaryPart = P
		end
	end
	M.Parent = Parent
	return M
end

-- The spatial rows' own geometry, away from the scene's parts (other rows move those, and the engine side runs each family group
-- in a scene of its own while ree runs them all in one): a stack of 24 coincident parts, and a line of 10 parts 3 studs apart
local SpatialAt = Vector3.new(1000, 500, 0)
local function spatial(S)
	if not S.Spatial then
		local Folder = Instance.new("Folder")
		Folder.Name = "ParitySpatial"
		local Stack = {}
		for I = 1, 24 do
			local P = part(Folder, "Stack" .. I)
			P.CFrame = CFrame.new(SpatialAt)
			Stack[I] = P
		end
		local Line = Instance.new("Model")
		Line.Name = "Line"
		for I = 1, 10 do
			local P = part(Line, "L" .. I)
			P.CFrame = CFrame.new(SpatialAt + Vector3.new(I * 3, 0, 20))
		end
		Line.Parent = Folder
		Folder.Parent = workspace
		S.Spatial = { Stack = Stack, Line = Line, First = Line:FindFirstChild("L1"), Last = Line:FindFirstChild("L10") }
	end
	return S.Spatial
end

local Scene
local function scene(Options)
	if Scene then
		return Scene
	end
	local S = {}
	local Root = Instance.new("Folder")
	Root.Name = "ParityScene"
	Root.Parent = workspace
	S.Root = Root
	S.Part = part(Root, "Part")
	S.Part2 = part(Root, "Part2")
	S.Part:SetAttribute("Number", 1)
	S.Part:SetAttribute("Text", "a")
	S.Part:SetAttribute("Flag", true)
	S.Part:SetAttribute("Vector", Vector3.one)
	S.Part:SetAttribute("Color", Color3.new(1, 0, 0))
	S.Part:AddTag("ParityTag")
	S.MeshPart = Instance.new("MeshPart")
	S.MeshPart.Anchored = true
	S.MeshPart.Parent = Root
	S.Bone = Instance.new("Bone")
	S.Bone.Parent = S.MeshPart
	S.Attachment = Instance.new("Attachment")
	S.Attachment.Parent = S.Part
	S.Attachment1 = Instance.new("Attachment")
	S.Attachment1.Position = Vector3.new(0, 2, 0)
	S.Attachment1.Parent = S.Part
	S.Motor = Instance.new("Motor6D")
	S.Motor.Part0 = S.Part
	S.Motor.Part1 = S.Part2
	S.Motor.Parent = S.Part
	S.Weld = Instance.new("Weld")
	S.Weld.Part0 = S.Part
	S.Weld.Part1 = S.Part2
	S.Weld.Parent = S.Part2
	S.Emitter = Instance.new("ParticleEmitter")
	S.Emitter.Enabled = false
	S.Emitter.Parent = S.Part
	S.Beam = Instance.new("Beam")
	S.Beam.Attachment0 = S.Attachment
	S.Beam.Attachment1 = S.Attachment1
	S.Beam.Parent = S.Part
	S.Light = Instance.new("PointLight")
	S.Light.Parent = S.Part
	S.Highlight = Instance.new("Highlight")
	S.Highlight.Enabled = false
	S.Highlight.Parent = Root
	S.Model = model(Root, 10)
	for I = 1, 10 do
		local P = part(Root, "Tagged" .. I)
		P:AddTag("ParityTagged")
	end
	S.Loose = part(nil, "Loose")
	S.LooseA = Instance.new("Folder")
	S.LooseB = Instance.new("Folder")
	S.PlacedA = Instance.new("Folder")
	S.PlacedA.Name = "PlacedA"
	S.PlacedA.Parent = Root
	S.PlacedB = Instance.new("Folder")
	S.PlacedB.Name = "PlacedB"
	S.PlacedB.Parent = Root
	S.Children20 = Instance.new("Folder")
	S.Children20.Name = "Children20"
	for I = 1, 20 do
		local C = Instance.new("Folder")
		C.Name = "Child" .. I
		C.Parent = S.Children20
	end
	S.Children20.Parent = Root
	S.Tree100 = tree(Root, "Tree100", 10, 2, "Part")
	S.Tree1000 = tree(Root, "Tree1000", 10, 3, "Folder")
	S.Leaf = S.Tree100:FindFirstChild("N1_1"):FindFirstChild("N2_1")
	S.Event = Instance.new("BindableEvent")
	S.Event.Parent = Root
	S.Camera = workspace.CurrentCamera

	local Player = Players.LocalPlayer
	local PlayerGui = Options.GuiParent or (Player and (Player:FindFirstChildOfClass("PlayerGui") or Player:WaitForChild("PlayerGui", 10)))
	local Gui = Instance.new("ScreenGui")
	Gui.Name = "ParityGui"
	Gui.ResetOnSpawn = false
	Gui.Parent = PlayerGui
	S.Gui = Gui
	S.Frame = Instance.new("Frame")
	S.Frame.Size = UDim2.fromOffset(200, 100)
	S.Frame.Parent = Gui
	S.Label = Instance.new("TextLabel")
	S.Label.Text = "12"
	S.Label.Parent = Gui
	S.Image = Instance.new("ImageLabel")
	S.Image.Parent = Gui
	S.ImageButton = Instance.new("ImageButton")
	S.ImageButton.Parent = Gui
	S.Gradient = Instance.new("UIGradient")
	S.Gradient.Parent = S.Frame
	S.Stroke = Instance.new("UIStroke")
	S.Stroke.Parent = S.Frame
	S.Scale = Instance.new("UIScale")
	S.Scale.Parent = S.Frame
	S.Card = guiTree(Gui)
	Scene = S
	return S
end

------------------------------------------------------------------------------------------------ workloads

-- loop: what an empty loop step costs on each side (every other row includes its own loop)
W("loop", "empty", 200000, function(N)
	for _ = 1, N do
	end
end)
W("loop", "os.clock", 50000, function(N)
	for _ = 1, N do
		local _ = os.clock()
	end
end)
W("loop", "tick", 50000, function(N)
	for _ = 1, N do
		local _ = tick()
	end
end)

-- read: one property of one instance, read N times
local Reads = {
	{ "Part", "CFrame" }, { "Part", "Position" }, { "Part", "Size" }, { "Part", "Transparency" }, { "Part", "Color" },
	{ "Part", "Anchored" }, { "Part", "Name" }, { "Part", "Parent" }, { "Part", "Material" }, { "Part", "ClassName" },
	{ "Model", "PrimaryPart" }, { "Attachment", "CFrame" }, { "Attachment", "WorldPosition" }, { "Motor", "Transform" },
	{ "Motor", "C0" }, { "Bone", "Transform" }, { "Frame", "Size" }, { "Frame", "Position" },
	{ "Frame", "BackgroundTransparency" }, { "Frame", "Visible" }, { "Frame", "AbsoluteSize" }, { "Label", "Text" },
	{ "Image", "Image" }, { "Gradient", "Offset" }, { "Gradient", "Color" }, { "Emitter", "Rate" }, { "Camera", "CFrame" },
	{ "Light", "Brightness" }, { "Emitter", "Transparency" }, { "Emitter", "Color" }, { "Label", "TextColor3" },
	{ "Frame", "BackgroundColor3" }, { "Attachment", "Position" }, { "Part", "Orientation" }, { "Gradient", "Transparency" },
	{ "Label", "TextSize" },
}
for _, Row in Reads do
	local Key, Property = Row[1], Row[2]
	W("read", Key .. "." .. Property, 20000, function(N, S)
		local I = S[Key]
		for _ = 1, N do
			local _ = I[Property]
		end
	end)
end
W("read", "workspace.CurrentCamera", 20000, function(N)
	local Space = workspace
	for _ = 1, N do
		local _ = Space.CurrentCamera
	end
end)
W("read", "Players.LocalPlayer", 20000, function(N)
	for _ = 1, N do
		local _ = Players.LocalPlayer
	end
end)
W("read", "Part.child", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P.Attachment
	end
end)
-- a GUI object's rect as a visibility test reads it: the property and both of its components
W("read", "Frame.AbsolutePosition.XY", 20000, function(N, S)
	local F = S.Frame
	for _ = 1, N do
		local P = F.AbsolutePosition
		local _ = P.X + P.Y
	end
end)

-- write: one property of one instance, alternating between two values (N writes)
local c1 = CFrame.new(1, 200, 3) * CFrame.Angles(0.3, 0.4, 0.5)
local c2 = CFrame.new(4, 205, 6) * CFrame.Angles(0.6, 0.1, 0.2)
local Writes = {
	{ "Part", "CFrame", c1, c2 }, { "Part", "Position", Vector3.new(0, 200, 0), Vector3.new(1, 201, 1) },
	{ "Part", "Orientation", Vector3.new(0, 10, 0), Vector3.new(0, 20, 0) }, { "Part", "Size", Vector3.new(2, 2, 2), Vector3.new(3, 2, 2) },
	{ "Part", "Color", Color3.new(1, 0, 0), Color3.new(0, 1, 0) }, { "Part", "Transparency", 0.25, 0.75 },
	{ "Part", "Name", "A", "B" }, { "Part", "CanCollide", true, false }, { "Part", "CastShadow", true, false },
	{ "Part", "Material", Enum.Material.Plastic, Enum.Material.Neon },
	{ "Loose", "CFrame", c1, c2 }, { "Loose", "Size", Vector3.new(2, 2, 2), Vector3.new(3, 2, 2) }, { "Loose", "Transparency", 0.25, 0.75 },
	{ "Attachment", "CFrame", CFrame.new(0, 1, 0), CFrame.new(0, 2, 0) }, { "Motor", "Transform", c1, c2 },
	{ "Motor", "C0", CFrame.new(0, 1, 0), CFrame.new(0, 2, 0) }, { "Bone", "Transform", c1, c2 },
	{ "Frame", "Size", UDim2.fromOffset(200, 100), UDim2.fromOffset(210, 100) },
	{ "Frame", "Position", UDim2.fromOffset(0, 0), UDim2.fromOffset(5, 5) }, { "Frame", "BackgroundTransparency", 0.25, 0.75 },
	{ "Frame", "Visible", true, false }, { "Frame", "Rotation", 0, 10 }, { "Frame", "BackgroundColor3", Color3.new(1, 0, 0), Color3.new(0, 1, 0) },
	{ "Frame", "ZIndex", 1, 2 }, { "Label", "Text", "12", "34" }, { "Label", "TextTransparency", 0.25, 0.75 },
	{ "Label", "TextColor3", Color3.new(1, 0, 0), Color3.new(0, 1, 0) }, { "Image", "ImageTransparency", 0.25, 0.75 },
	{ "Image", "ImageColor3", Color3.new(1, 0, 0), Color3.new(0, 1, 0) },
	{ "Image", "Image", "rbxasset://textures/ui/GuiImagePlaceholder.png", "rbxasset://textures/whiteCircle.png" },
	{ "Gradient", "Offset", Vector2.new(0, 0), Vector2.new(0.5, 0) }, { "Gradient", "Rotation", 0, 90 },
	{ "Gradient", "Color", ColorSequence.new(Color3.new(1, 0, 0), Color3.new(0, 0, 1)), ColorSequence.new(Color3.new(0, 1, 0), Color3.new(1, 1, 0)) },
	{ "Gradient", "Transparency", NumberSequence.new(0, 1), NumberSequence.new(1, 0) }, { "Stroke", "Thickness", 1, 2 },
	{ "Scale", "Scale", 1, 1.1 }, { "Emitter", "Rate", 10, 20 }, { "Emitter", "Enabled", true, false },
	{ "Beam", "Width0", 1, 2 }, { "Light", "Brightness", 1, 2 }, { "Highlight", "FillTransparency", 0.25, 0.75 },
	{ "Camera", "CFrame", CFrame.new(0, 10, 20), CFrame.new(0, 11, 20) },
	{ "ImageButton", "Image", "rbxasset://textures/ui/GuiImagePlaceholder.png", "rbxasset://textures/whiteCircle.png" },
	{ "Weld", "C0", CFrame.new(0, 1, 0), CFrame.new(0, 2, 0) }, { "Attachment", "Position", Vector3.new(0, 1, 0), Vector3.new(0, 2, 0) },
	{ "Frame", "BorderColor3", Color3.new(1, 0, 0), Color3.new(0, 1, 0) }, { "Part", "BrickColor", BrickColor.new("Bright red"), BrickColor.new("Bright blue") },
	{ "Part", "Reflectance", 0.25, 0.75 },
}
for _, Row in Writes do
	local Key, Property, A, B = Row[1], Row[2], Row[3], Row[4]
	W("write", Key .. "." .. Property, 10000, function(N, S)
		local I = S[Key]
		for _ = 1, N // 2 do
			I[Property] = A
			I[Property] = B
		end
	end)
end
W("write", "Label.Text.letters", 4000, function(N, S)
	local I = S.Label
	for _ = 1, N // 2 do
		I.Text = "Hello"
		I.Text = "World"
	end
end)
W("write", "Part.CFrame.same", 10000, function(N, S)
	local I = S.Part
	local C = I.CFrame
	for _ = 1, N do
		I.CFrame = C
	end
end)
W("write", "Part.Transparency.same", 10000, function(N, S)
	local I = S.Part
	local T = I.Transparency
	for _ = 1, N do
		I.Transparency = T
	end
end)
-- the first touch of each instance in a frame: one write to each of 100 parts
W("write", "roster100.Transparency", 100, function(N, S)
	local Parts = S.Roster
	for I = 1, N do
		Parts[I].Transparency = if Parts[I].Transparency == 0 then 0.5 else 0
	end
end, {
	Prepare = function(_, S)
		if not S.Roster then
			S.Roster = {}
			for I = 1, 100 do
				S.Roster[I] = part(S.Root, "Roster" .. I)
			end
		end
	end,
})

-- the first touch of each instance in a frame, on static instances: one read or write each of 100 parts or 100 frames
local function rosterParts(S)
	if not S.Roster then
		S.Roster = {}
		for I = 1, 100 do
			S.Roster[I] = part(S.Root, "Roster" .. I)
		end
	end
	return S.Roster
end
local function rosterFrames(S)
	if not S.FrameRoster then
		S.FrameRoster = {}
		for I = 1, 100 do
			local F = Instance.new("Frame")
			F.Name = "Roster" .. I
			F.Size = UDim2.fromOffset(10, 10)
			F.Parent = S.Gui
			S.FrameRoster[I] = F
		end
	end
	return S.FrameRoster
end
W("read", "roster100.Part.Transparency", 100, function(N, S)
	local Parts = rosterParts(S)
	for I = 1, N do
		local _ = Parts[I].Transparency
	end
end)
W("read", "roster100.Part.CFrame", 100, function(N, S)
	local Parts = rosterParts(S)
	for I = 1, N do
		local _ = Parts[I].CFrame
	end
end)
W("read", "roster100.Frame.AbsolutePosition", 100, function(N, S)
	local Frames = rosterFrames(S)
	for I = 1, N do
		local _ = Frames[I].AbsolutePosition
	end
end)
W("write", "roster100.Part.CFrame", 100, function(N, S)
	local Parts = rosterParts(S)
	for I = 1, N do
		Parts[I].CFrame = if Parts[I].Position.X == 0 then c1 else CFrame.new(0, 200, 0)
	end
end)
W("write", "roster100.Frame.BackgroundTransparency", 100, function(N, S)
	local Frames = rosterFrames(S)
	for I = 1, N do
		Frames[I].BackgroundTransparency = if Frames[I].BackgroundTransparency == 0 then 0.5 else 0
	end
end)

-- method: one call on one instance, N times
local function method(Name, N, Fn)
	W("method", Name, N, Fn)
end
method("IsA.hit", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:IsA("BasePart")
	end
end)
method("IsA.miss", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:IsA("GuiObject")
	end
end)
method("GetFullName", 10000, function(N, S)
	local P = S.Leaf
	for _ = 1, N do
		local _ = P:GetFullName()
	end
end)
method("IsDescendantOf", 20000, function(N, S)
	local P = S.Leaf
	for _ = 1, N do
		local _ = P:IsDescendantOf(workspace)
	end
end)
method("GetService", 20000, function(N)
	for _ = 1, N do
		local _ = game:GetService("RunService")
	end
end)
method("GetPlayers", 10000, function(N)
	for _ = 1, N do
		local _ = Players:GetPlayers()
	end
end)
-- ReflectionService: every class's entry, a filtered list, and one class (each call makes new frozen tables)
method("ReflectionService.GetClasses", 20, function(N)
	local Reflection = game:GetService("ReflectionService")
	for _ = 1, N do
		local _ = Reflection:GetClasses()
	end
end)
method("ReflectionService.GetClasses.IsA", 200, function(N)
	local Reflection = game:GetService("ReflectionService")
	local Filter = { IsA = "BasePart" }
	for _ = 1, N do
		local _ = Reflection:GetClasses(Filter)
	end
end)
method("ReflectionService.GetClass", 2000, function(N)
	local Reflection = game:GetService("ReflectionService")
	for _ = 1, N do
		local _ = Reflection:GetClass("Part")
	end
end)
method("Model.GetPivot", 10000, function(N, S)
	local M = S.Model
	for _ = 1, N do
		local _ = M:GetPivot()
	end
end)
method("Model.PivotTo", 2000, function(N, S)
	local M = S.Model
	for _ = 1, N // 2 do
		M:PivotTo(c1)
		M:PivotTo(c2)
	end
end)
method("Part.PivotTo", 4000, function(N, S)
	local P = S.Part2
	for _ = 1, N // 2 do
		P:PivotTo(c1)
		P:PivotTo(c2)
	end
end)
method("Model.GetBoundingBox", 4000, function(N, S)
	local M = S.Model
	for _ = 1, N do
		local _ = M:GetBoundingBox()
	end
end)
method("Model.GetExtentsSize", 4000, function(N, S)
	local M = S.Model
	for _ = 1, N do
		local _ = M:GetExtentsSize()
	end
end)
method("BulkMoveTo10", 2000, function(N, S)
	local Parts = S.Model:GetChildren()
	local A, B = {}, {}
	for I, P in Parts do
		A[I] = c1 * CFrame.new(I, 0, 0)
		B[I] = c2 * CFrame.new(I, 0, 0)
	end
	for _ = 1, N // 2 do
		workspace:BulkMoveTo(Parts, A, Enum.BulkMoveMode.FireCFrameChanged)
		workspace:BulkMoveTo(Parts, B, Enum.BulkMoveMode.FireCFrameChanged)
	end
end)
method("GetPropertyChangedSignal", 10000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:GetPropertyChangedSignal("Transparency")
	end
end)
method("Emitter.Emit1", 2000, function(N, S)
	local E = S.Emitter
	for _ = 1, N do
		E:Emit(1)
	end
end, nil)
method("Camera.WorldToViewportPoint", 10000, function(N, S)
	local C = S.Camera
	local V = Vector3.new(1, 2, 3)
	for _ = 1, N do
		local _ = C:WorldToViewportPoint(V)
	end
end)
method("workspace.Raycast", 4000, function(N, S)
	local Params = RaycastParams.new()
	Params.FilterType = Enum.RaycastFilterType.Include
	Params.FilterDescendantsInstances = { spatial(S).Stack[1] }
	local From, Direction = SpatialAt + Vector3.new(0, 10, 0), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Raycast(From, Direction, Params)
	end
end)
method("workspace.GetPartBoundsInBox", 2000, function(N, S)
	spatial(S)
	local Params = OverlapParams.new()
	local C, Size = CFrame.new(SpatialAt), Vector3.new(4, 4, 4)
	for _ = 1, N do
		local _ = workspace:GetPartBoundsInBox(C, Size, Params)
	end
end)
method("CollectionService.GetTagged10", 10000, function(N)
	for _ = 1, N do
		local _ = CollectionService:GetTagged("ParityTagged")
	end
end)
method("HasTag", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:HasTag("ParityTag")
	end
end)
method("AddTag.RemoveTag", 4000, function(N, S)
	local P = S.Part2
	for _ = 1, N // 2 do
		P:AddTag("ParityTemp")
		P:RemoveTag("ParityTemp")
	end
end)
method("GetTags", 10000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:GetTags()
	end
end)
method("TextService.GetTextSize", 2000, function(N)
	local TextService = game:GetService("TextService")
	local Bounds = Vector2.new(1000, 1000)
	for I = 1, N do
		local _ = TextService:GetTextSize(if I % 2 == 0 then "Hello" else "World", 14, Enum.Font.SourceSans, Bounds)
	end
end)

method("workspace.GetPartBoundsInBox.empty", 4000, function(N)
	local Params = OverlapParams.new()
	local C, Size = CFrame.new(5000, 5000, 5000), Vector3.new(4, 4, 4)
	for _ = 1, N do
		local _ = workspace:GetPartBoundsInBox(C, Size, Params)
	end
end)
method("workspace.GetPartBoundsInBox.empty40", 2000, function(N)
	local Params = OverlapParams.new()
	local C, Size = CFrame.new(5000, 5000, 5000), Vector3.new(40, 40, 40)
	for _ = 1, N do
		local _ = workspace:GetPartBoundsInBox(C, Size, Params)
	end
end)
method("workspace.GetPartBoundsInRadius", 2000, function(N, S)
	spatial(S)
	local Params = OverlapParams.new()
	local At = SpatialAt + Vector3.new(30, 0, 20)
	for _ = 1, N do
		local _ = workspace:GetPartBoundsInRadius(At, 2, Params)
	end
end)
method("workspace.GetPartsInPart", 1000, function(N, S)
	local Params = OverlapParams.new()
	local P = spatial(S).First
	for _ = 1, N do
		local _ = workspace:GetPartsInPart(P, Params)
	end
end)
method("workspace.Raycast.miss", 4000, function(N)
	local Params = RaycastParams.new()
	local From, Direction = Vector3.new(5000, 5000, 5000), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Raycast(From, Direction, Params)
	end
end)
method("workspace.Raycast.exclude", 2000, function(N, S)
	local Params = RaycastParams.new()
	Params.FilterType = Enum.RaycastFilterType.Exclude
	Params.FilterDescendantsInstances = { spatial(S).Line }
	local From, Direction = SpatialAt + Vector3.new(30, 10, 20), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Raycast(From, Direction, Params)
	end
end)
-- what a ray and an overlap cost by what they meet: the stack, one part of the line, nothing; an Include filter that admits a
-- part the ray does not reach (Meta.queries reports what each side's queries find)
method("workspace.Raycast.hit_stack", 4000, function(N, S)
	spatial(S)
	local Params = RaycastParams.new()
	local From, Direction = SpatialAt + Vector3.new(0, 10, 0), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Raycast(From, Direction, Params)
	end
end)
method("workspace.Raycast.hit_one", 4000, function(N, S)
	spatial(S)
	local Params = RaycastParams.new()
	local From, Direction = SpatialAt + Vector3.new(30, 10, 20), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Raycast(From, Direction, Params)
	end
end)
method("workspace.Raycast.include_miss", 4000, function(N, S)
	local Params = RaycastParams.new()
	Params.FilterType = Enum.RaycastFilterType.Include
	Params.FilterDescendantsInstances = { spatial(S).Stack[1] }
	local From, Direction = Vector3.new(5000, 5000, 5000), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Raycast(From, Direction, Params)
	end
end)
-- the shape casts, which the clock priced as a ray until they were measured
method("workspace.Spherecast.miss", 4000, function(N)
	local Params = RaycastParams.new()
	local From, Direction = Vector3.new(5000, 5000, 5000), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Spherecast(From, 1, Direction, Params)
	end
end)
method("workspace.Spherecast.hit_stack", 4000, function(N, S)
	spatial(S)
	local Params = RaycastParams.new()
	local From, Direction = SpatialAt + Vector3.new(0, 10, 0), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Spherecast(From, 1, Direction, Params)
	end
end)
method("workspace.Blockcast.miss", 4000, function(N)
	local Params = RaycastParams.new()
	local From, Size, Direction = CFrame.new(5000, 5000, 5000), Vector3.new(2, 2, 2), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Blockcast(From, Size, Direction, Params)
	end
end)
method("workspace.Blockcast.hit_stack", 4000, function(N, S)
	spatial(S)
	local Params = RaycastParams.new()
	local From, Size, Direction = CFrame.new(SpatialAt + Vector3.new(0, 10, 0)), Vector3.new(2, 2, 2), Vector3.new(0, -20, 0)
	for _ = 1, N do
		local _ = workspace:Blockcast(From, Size, Direction, Params)
	end
end)
method("workspace.GetPartBoundsInBox.model10", 2000, function(N, S)
	spatial(S)
	local Params = OverlapParams.new()
	local C, Size = CFrame.new(SpatialAt + Vector3.new(16.5, 0, 20)), Vector3.new(30, 2, 2)
	for _ = 1, N do
		local _ = workspace:GetPartBoundsInBox(C, Size, Params)
	end
end)
method("workspace.GetPartsInPart.stack", 1000, function(N, S)
	local Params = OverlapParams.new()
	local P = spatial(S).Stack[1]
	for _ = 1, N do
		local _ = workspace:GetPartsInPart(P, Params)
	end
end)
-- GetPartsInPart of a part that touches nothing: the call's own work (a part far from everything, a big one in empty space, a
-- ball, one beside the line, and one whose Include filter admits only a part it does not touch; Meta.queries says what each finds)
local function loner(S, Key)
	local Spatial = spatial(S)
	if not Spatial[Key] then
		local P = part(nil, "Lone" .. Key)
		if Key == "Alone" then
			P.CFrame = CFrame.new(SpatialAt + Vector3.new(0, 0, 200))
		elseif Key == "Big" then
			P.Size = Vector3.new(20, 20, 20)
			P.CFrame = CFrame.new(SpatialAt + Vector3.new(0, 0, -100))
		elseif Key == "Ball" then
			P.Shape = Enum.PartType.Ball
			P.Size = Vector3.new(4, 4, 4)
			P.CFrame = CFrame.new(SpatialAt + Vector3.new(0, 0, 100))
		elseif Key == "Six" or Key == "Twelve" or Key == "Forty" then
			local Side = if Key == "Six" then 6 elseif Key == "Twelve" then 12 else 40
			P.Size = Vector3.new(Side, Side, Side)
			P.CFrame = CFrame.new(SpatialAt + Vector3.new(0, 0, if Key == "Six" then 300 elseif Key == "Twelve" then 400 else 500))
		elseif Key == "Rod" then
			P.Size = Vector3.new(2, 2, 40)
			P.CFrame = CFrame.new(SpatialAt + Vector3.new(0, 0, 600))
		elseif Key == "Slab" then
			P.Size = Vector3.new(40, 2, 40)
			P.CFrame = CFrame.new(SpatialAt + Vector3.new(0, 0, 700))
		else
			P.CFrame = CFrame.new(SpatialAt + Vector3.new(4.5, 0, 23))
		end
		P.Parent = Spatial.Line.Parent
		Spatial[Key] = P
	end
	return Spatial[Key]
end
for _, Key in { "Alone", "Big", "Ball", "Beside", "Six", "Twelve", "Forty", "Rod", "Slab" } do
	method("workspace.GetPartsInPart.none." .. string.lower(Key), 2000, function(N, S)
		local Params = OverlapParams.new()
		local P = loner(S, Key)
		for _ = 1, N do
			local _ = workspace:GetPartsInPart(P, Params)
		end
	end)
end
method("workspace.GetPartsInPart.none.include", 2000, function(N, S)
	local Params = OverlapParams.new()
	Params.FilterType = Enum.RaycastFilterType.Include
	Params.FilterDescendantsInstances = { spatial(S).Stack[1] }
	local P = loner(S, "Alone")
	for _ = 1, N do
		local _ = workspace:GetPartsInPart(P, Params)
	end
end)

local function freshFolders(Count, Tagged)
	local Folders = table.create(Count)
	for I = 1, Count do
		Folders[I] = Instance.new("Folder")
		if Tagged then
			Folders[I]:AddTag(Tagged)
		end
	end
	return Folders
end
local function destroyAll(Objects)
	for _, Object in Objects do
		Object:Destroy()
	end
end
W("method", "CollectionService.AddTag", 4000, function(N, _, Folders)
	for I = 1, N do
		CollectionService:AddTag(Folders[I], "ParityAdded")
	end
end, {
	Prepare = function(N)
		return freshFolders(N)
	end,
	Cleanup = destroyAll,
})
W("method", "CollectionService.RemoveTag", 4000, function(N, _, Folders)
	for I = 1, N do
		CollectionService:RemoveTag(Folders[I], "ParityRemoved")
	end
end, {
	Prepare = function(N)
		return freshFolders(N, "ParityRemoved")
	end,
	Cleanup = destroyAll,
})
method("HasTag.miss", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:HasTag("Nope")
	end
end)
method("GetTags2", 10000, function(N, S)
	local P = S.Part2
	for _ = 1, N do
		local _ = P:GetTags()
	end
end)
method("IsDescendantOf.shallow", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:IsDescendantOf(S.Root)
	end
end)

-- find: lookups and listings by size
W("find", "FindFirstChild.first20", 20000, function(N, S)
	local F = S.Children20
	for _ = 1, N do
		local _ = F:FindFirstChild("Child1")
	end
end)
W("find", "FindFirstChild.last20", 20000, function(N, S)
	local F = S.Children20
	for _ = 1, N do
		local _ = F:FindFirstChild("Child20")
	end
end)
W("find", "FindFirstChild.miss20", 20000, function(N, S)
	local F = S.Children20
	for _ = 1, N do
		local _ = F:FindFirstChild("Nope")
	end
end)
W("find", "FindFirstChild.recursive100", 4000, function(N, S)
	local F = S.Tree100
	for _ = 1, N do
		local _ = F:FindFirstChild("N2_10", true)
	end
end)
W("find", "FindFirstChild.recursive_miss1000", 400, function(N, S)
	local F = S.Tree1000
	for _ = 1, N do
		local _ = F:FindFirstChild("Nope", true)
	end
end)
W("find", "FindFirstChildOfClass", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:FindFirstChildOfClass("ParticleEmitter")
	end
end)
W("find", "FindFirstChildWhichIsA", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:FindFirstChildWhichIsA("JointInstance")
	end
end)
W("find", "FindFirstAncestor", 20000, function(N, S)
	local P = S.Leaf
	for _ = 1, N do
		local _ = P:FindFirstAncestor("ParityScene")
	end
end)
W("find", "FindFirstAncestorOfClass", 20000, function(N, S)
	local P = S.Leaf
	for _ = 1, N do
		local _ = P:FindFirstAncestorOfClass("Workspace")
	end
end)
W("find", "WaitForChild.present", 20000, function(N, S)
	local F = S.Children20
	for _ = 1, N do
		local _ = F:WaitForChild("Child10")
	end
end)
W("find", "GetChildren0", 20000, function(N, S)
	local P = S.Leaf
	for _ = 1, N do
		local _ = P:GetChildren()
	end
end)
W("find", "GetChildren20", 10000, function(N, S)
	local F = S.Children20
	for _ = 1, N do
		local _ = F:GetChildren()
	end
end)
W("find", "GetDescendants100", 2000, function(N, S)
	local F = S.Tree100
	for _ = 1, N do
		local _ = F:GetDescendants()
	end
end)
W("find", "GetDescendants1000", 200, function(N, S)
	local F = S.Tree1000
	for _ = 1, N do
		local _ = F:GetDescendants()
	end
end)
W("find", "QueryDescendants.Part100", 2000, function(N, S)
	local F = S.Tree100
	for _ = 1, N do
		local _ = F:QueryDescendants(">>Part")
	end
end)
W("find", "GetChildren20.iterate_Name", 4000, function(N, S)
	local F = S.Children20
	local Count = 0
	for _ = 1, N do
		for _, C in F:GetChildren() do
			if C.Name == "Child7" then
				Count += 1
			end
		end
	end
end)

-- attr: attributes
W("attr", "GetAttribute.hit", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:GetAttribute("Number")
	end
end)
W("attr", "GetAttribute.miss", 20000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:GetAttribute("Nope")
	end
end)
W("attr", "GetAttributes5", 10000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		local _ = P:GetAttributes()
	end
end)
W("attr", "SetAttribute.number", 10000, function(N, S)
	local P = S.Part
	for _ = 1, N // 2 do
		P:SetAttribute("Number", 1)
		P:SetAttribute("Number", 2)
	end
end)
W("attr", "SetAttribute.string", 10000, function(N, S)
	local P = S.Part
	for _ = 1, N // 2 do
		P:SetAttribute("Text", "a")
		P:SetAttribute("Text", "b")
	end
end)
W("attr", "SetAttribute.same", 10000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		P:SetAttribute("Flag", true)
	end
end)
W("attr", "SetAttribute.add_remove", 4000, function(N, S)
	local P = S.Part2
	for _ = 1, N // 2 do
		P:SetAttribute("Temp", 1)
		P:SetAttribute("Temp", nil)
	end
end)

-- create: Instance.new by class (kept until the round ends, then destroyed outside the timed span)
local Created = {}
local function destroyCreated()
	for I, Object in Created do
		Object:Destroy()
		Created[I] = nil
	end
end
for _, Class in { "Part", "MeshPart", "Folder", "Model", "Attachment", "Frame", "TextLabel", "ImageLabel", "UIGradient", "UICorner",
	"UIListLayout", "ParticleEmitter", "Beam", "Motor6D", "WeldConstraint", "NumberValue", "BindableEvent", "Sound", "Highlight",
	"PointLight", "ScreenGui", "Bone", "Decal", "TextButton", "ImageButton", "UIStroke", "UIPadding", "Trail", "SpotLight", "BillboardGui",
	"ObjectValue", "StringValue", "UIScale", "Weld", "Sparkles" } do
	W("create", Class, 1000, function(N)
		for I = 1, N do
			Created[I] = Instance.new(Class)
		end
	end, { Cleanup = destroyCreated })
end

-- clone: Clone of one instance or a tree (copies kept until the round ends)
local Templates = {
	Part = function(S)
		return S.Part2
	end,
	MeshPart = function(S)
		return S.MeshPart
	end,
	Model10 = function(S)
		return S.Model
	end,
	EffectPart = function(S)
		S.EffectTemplate = S.EffectTemplate or effectPart(nil)
		return S.EffectTemplate
	end,
	GuiCard = function(S)
		return S.Card
	end,
	Folder20Values = function(S)
		if not S.Values then
			S.Values = Instance.new("Folder")
			for I = 1, 20 do
				local V = Instance.new("NumberValue")
				V.Name = "V" .. I
				V.Parent = S.Values
			end
		end
		return S.Values
	end,
	ParticleEmitter = function(S)
		return S.Emitter
	end,
	TextLabel = function(S)
		return S.Label
	end,
	Model5 = function(S)
		S.Model5 = S.Model5 or model(nil, 5)
		return S.Model5
	end,
	Folder5Parts = function(S)
		if not S.Folder5Parts then
			S.Folder5Parts = Instance.new("Folder")
			for I = 1, 5 do
				part(S.Folder5Parts, "P" .. I)
			end
		end
		return S.Folder5Parts
	end,
	Beam = function(S)
		return S.Beam
	end,
	UIGradient = function(S)
		return S.Gradient
	end,
	Attachment = function(S)
		return S.Attachment
	end,
}
for _, Name in { "Part", "MeshPart", "Model10", "EffectPart", "GuiCard", "Folder20Values", "ParticleEmitter", "TextLabel", "Model5",
	"Folder5Parts", "Beam", "UIGradient", "Attachment" } do
	local N = if Name == "Model10" or Name == "GuiCard" or Name == "Folder20Values" or Name == "EffectPart" or Name == "Model5" or Name == "Folder5Parts" then 200 else 1000
	W("clone", Name, N, function(Count, S)
		local Template = Templates[Name](S)
		for I = 1, Count do
			Created[I] = Template:Clone()
		end
	end, { Cleanup = destroyCreated })
end

-- parent: moves into and out of the DataModel and inside or outside it (N moves)
local function mover(Name, N, Make, Into, Away)
	W("parent", Name, N, function(Count, S, Object)
		local A, B = Into(S), Away(S)
		for _ = 1, Count // 2 do
			Object.Parent = A
			Object.Parent = B
		end
	end, {
		Prepare = function(_, S)
			return Make(S)
		end,
		Cleanup = function(Object)
			Object:Destroy()
		end,
	})
end
mover("Part.enter_leave", 2000, function()
	return part(nil, "Mover")
end, function(S)
	return S.PlacedA
end, function()
	return nil
end)
mover("Part.within_workspace", 4000, function(S)
	return part(S.PlacedB, "Mover")
end, function(S)
	return S.PlacedA
end, function(S)
	return S.PlacedB
end)
mover("Part.outside", 4000, function(S)
	return part(S.LooseB, "Mover")
end, function(S)
	return S.LooseA
end, function(S)
	return S.LooseB
end)
mover("Model10.enter_leave", 400, function()
	return model(nil, 10)
end, function(S)
	return S.PlacedA
end, function()
	return nil
end)
mover("GuiCard.enter_leave", 1000, function()
	return guiTree(nil)
end, function(S)
	return S.Gui
end, function()
	return nil
end)
mover("Folder20Values.enter_leave", 1000, function(S)
	return Templates.Folder20Values(S):Clone()
end, function()
	return ReplicatedStorage
end, function()
	return nil
end)
mover("Folder10Values.enter_leave", 1000, function()
	local F = Instance.new("Folder")
	for I = 1, 10 do
		local V = Instance.new("NumberValue")
		V.Name = "V" .. I
		V.Parent = F
	end
	return F
end, function()
	return ReplicatedStorage
end, function()
	return nil
end)
mover("Model3.enter_leave", 1000, function()
	return model(nil, 3)
end, function(S)
	return S.PlacedA
end, function()
	return nil
end)
mover("Folder.enter_leave", 4000, function()
	return Instance.new("Folder")
end, function(S)
	return S.PlacedA
end, function()
	return nil
end)
mover("Frame.enter_leave", 2000, function()
	local F = Instance.new("Frame")
	F.Size = UDim2.fromOffset(20, 20)
	return F
end, function(S)
	return S.Gui
end, function()
	return nil
end)
mover("Attachment.enter_leave", 4000, function(S)
	return Instance.new("Attachment")
end, function(S)
	return S.Part2
end, function()
	return nil
end)

-- destroy: Destroy of instances made before the round
local function destroyer(Name, N, Make)
	W("destroy", Name, N, function(Count, _, Objects)
		for I = 1, Count do
			Objects[I]:Destroy()
		end
	end, {
		Prepare = function(Count, S)
			local Objects = {}
			for I = 1, Count do
				Objects[I] = Make(S)
			end
			return Objects
		end,
	})
end
destroyer("Part.placed", 1000, function(S)
	return part(S.PlacedA, "Doomed")
end)
destroyer("Part.loose", 2000, function()
	return part(nil, "Doomed")
end)
destroyer("Model10.placed", 200, function(S)
	return model(S.PlacedA, 10)
end)
destroyer("GuiCard.placed", 400, function(S)
	return guiTree(S.Gui)
end)
destroyer("Folder.loose", 2000, function()
	return Instance.new("Folder")
end)
destroyer("Folder.placed", 2000, function(S)
	local F = Instance.new("Folder")
	F.Parent = S.PlacedA
	return F
end)
destroyer("Frame.placed", 1000, function(S)
	local F = Instance.new("Frame")
	F.Parent = S.Gui
	return F
end)

-- signal: connections, fires and the handlers they queue
local function noop() end
W("signal", "Changed.connect_disconnect", 4000, function(N, S)
	local Signal = S.Part.Changed
	for _ = 1, N do
		Signal:Connect(noop):Disconnect()
	end
end)
W("signal", "Bindable.connect_disconnect", 4000, function(N, S)
	local Signal = S.Event.Event
	for _ = 1, N do
		Signal:Connect(noop):Disconnect()
	end
end)
W("signal", "Bindable.once_disconnect", 4000, function(N, S)
	local Signal = S.Event.Event
	for _ = 1, N do
		Signal:Once(noop):Disconnect()
	end
end)
W("signal", "PropertySignal.connect_disconnect", 4000, function(N, S)
	local P = S.Part
	for _ = 1, N do
		P:GetPropertyChangedSignal("Transparency"):Connect(noop):Disconnect()
	end
end)
W("signal", "Heartbeat.connect_disconnect", 4000, function(N)
	local Signal = RunService.Heartbeat
	for _ = 1, N do
		Signal:Connect(noop):Disconnect()
	end
end)
W("signal", "Bindable.fire_unheard", 10000, function(N, S)
	local E = S.Event
	for _ = 1, N do
		E:Fire()
	end
end)
-- with one handler: the firer's own cost (the handler runs after the round's span)...
W("signal", "Bindable.fire_heard", 4000, function(N, S, State)
	local E = S.Event
	for _ = 1, N do
		E:Fire()
	end
end, {
	Prepare = function(_, S)
		return S.Event.Event:Connect(noop)
	end,
	Cleanup = function(Connection)
		task.wait()
		Connection:Disconnect()
	end,
})
-- ... and to the end of the last handler: the firer and every handler's thread (the round yields once to let them run)
local Stamp = 0
local function stamp()
	Stamp = os.clock()
end
W("signal", "Bindable.fire_heard.through_handlers", 2000, function(N, S)
	local E = S.Event
	local T0 = os.clock()
	for _ = 1, N do
		E:Fire()
	end
	task.wait()
	return Stamp - T0
end, {
	Measured = true,
	Prepare = function(_, S)
		return S.Event.Event:Connect(stamp)
	end,
	Cleanup = function(Connection)
		Connection:Disconnect()
	end,
})
W("signal", "write.Changed_heard", 4000, function(N, S)
	local P = S.Part2
	for _ = 1, N // 2 do
		P.Transparency = 0.25
		P.Transparency = 0.75
	end
end, {
	Prepare = function(_, S)
		return S.Part2.Changed:Connect(noop)
	end,
	Cleanup = function(Connection)
		task.wait()
		Connection:Disconnect()
	end,
})
W("signal", "write.PropertySignal_heard.through_handlers", 2000, function(N, S)
	local P = S.Part2
	local T0 = os.clock()
	for _ = 1, N // 2 do
		P.Transparency = 0.25
		P.Transparency = 0.75
	end
	task.wait()
	return Stamp - T0
end, {
	Measured = true,
	Prepare = function(_, S)
		return S.Part2:GetPropertyChangedSignal("Transparency"):Connect(stamp)
	end,
	Cleanup = function(Connection)
		Connection:Disconnect()
	end,
})
W("signal", "write.Attribute_heard.through_handlers", 2000, function(N, S)
	local P = S.Part2
	local T0 = os.clock()
	for _ = 1, N // 2 do
		P:SetAttribute("Heard", 1)
		P:SetAttribute("Heard", 2)
	end
	task.wait()
	return Stamp - T0
end, {
	Measured = true,
	Prepare = function(_, S)
		return S.Part2:GetAttributeChangedSignal("Heard"):Connect(stamp)
	end,
	Cleanup = function(Connection)
		Connection:Disconnect()
	end,
})
-- 100 Heartbeat handlers: the span from the first handler's stamp to the last, over 99 (one round a frame)
W("signal", "Heartbeat.handler_spacing", 99, function(_, _, State)
	State.Frame = 0
	local Before = State.Done
	repeat
		task.wait()
	until State.Done > Before
	return State.Span
end, {
	Measured = true,
	Prepare = function()
		local State = { Stamps = {}, Count = 0, Done = 0, Span = 0, Connections = {} }
		-- (whatever order the handlers run in: the 100th to run in a frame closes its span)
		local function handler()
			State.Count += 1
			State.Stamps[State.Count] = os.clock()
			if State.Count == 100 then
				State.Span = State.Stamps[100] - State.Stamps[1]
				State.Done += 1
				State.Count = 0
			end
		end
		for I = 1, 100 do
			State.Connections[I] = RunService.Heartbeat:Connect(handler)
		end
		task.wait()
		return State
	end,
	Cleanup = function(State)
		for _, C in State.Connections do
			C:Disconnect()
		end
	end,
})

-- task: threads
W("task", "spawn.noop", 4000, function(N)
	for _ = 1, N do
		task.spawn(noop)
	end
end)
W("task", "defer.noop.through_threads", 2000, function(N)
	local T0 = os.clock()
	for _ = 1, N do
		task.defer(noop)
	end
	task.defer(stamp)
	task.wait()
	return Stamp - T0
end, { Measured = true })
W("task", "delay.queue", 4000, function(N, _, Threads)
	for I = 1, N do
		Threads[I] = task.delay(60, noop)
	end
end, {
	Prepare = function()
		return {}
	end,
	Cleanup = function(Threads)
		for _, T in Threads do
			task.cancel(T)
		end
	end,
})
W("task", "delay.cancel", 4000, function(N)
	for _ = 1, N do
		task.cancel(task.delay(60, noop))
	end
end)
W("task", "coroutine.wrap_call", 4000, function(N)
	for _ = 1, N do
		coroutine.wrap(noop)()
	end
end)
W("task", "coroutine.create_resume", 4000, function(N)
	for _ = 1, N do
		coroutine.resume(coroutine.create(noop))
	end
end)
-- resumptions: 100 threads the engine resumes in one frame, the span from the first one's stamp to the last's over 99 (what each
-- resumption costs, with the few instructions each thread runs): parked in task.wait, delayed by task.delay(0), and waiting in a
-- signal's :Wait() for one Fire (src/cost.rs RESUMED)
local function spacing()
	local State = { Stamps = {}, Count = 0, Done = 0, Span = 0, Running = true }
	function State.stamp()
		State.Count += 1
		State.Stamps[State.Count] = os.clock()
		if State.Count == 100 then
			State.Span = State.Stamps[100] - State.Stamps[1]
			State.Done += 1
			State.Count = 0
		end
	end
	return State
end
local function awaitDone(State, Before)
	repeat
		task.wait()
	until State.Done > Before
	return State.Span
end
W("task", "wait.resume_spacing", 99, function(_, _, State)
	return awaitDone(State, State.Done)
end, {
	Measured = true,
	Prepare = function()
		local State = spacing()
		for _ = 1, 100 do
			task.spawn(function()
				while State.Running do
					task.wait()
					if State.Running then
						State.stamp()
					end
				end
			end)
		end
		task.wait()
		return State
	end,
	Cleanup = function(State)
		State.Running = false
		task.wait()
	end,
})
W("task", "delay.resume_spacing", 99, function(_, _, State)
	return awaitDone(State, 0)
end, {
	Measured = true,
	Prepare = function()
		local State = spacing()
		for _ = 1, 100 do
			task.delay(0, State.stamp)
		end
		return State
	end,
})
W("task", "signal_wait.resume_spacing", 99, function(_, S, State)
	S.Event:Fire()
	return awaitDone(State, State.Done)
end, {
	Measured = true,
	Prepare = function(_, S)
		local State = spacing()
		local Event = S.Event.Event
		for _ = 1, 100 do
			task.spawn(function()
				while State.Running do
					Event:Wait()
					if State.Running then
						State.stamp()
					end
				end
			end)
		end
		return State
	end,
	Cleanup = function(State, S)
		State.Running = false
		S.Event:Fire()
		task.wait()
	end,
})

-- tween: TweenService
local Info = TweenInfo.new(1)
W("tween", "TweenInfo.new", 20000, function(N)
	for I = 1, N do
		local _ = TweenInfo.new(I * 0.001)
	end
end)
W("tween", "Create", 2000, function(N, S)
	local P = S.Part2
	local Goal = { Transparency = 0.5 }
	for _ = 1, N do
		local _ = TweenService:Create(P, Info, Goal)
	end
end)
W("tween", "Create.Play.Cancel", 1000, function(N, S)
	local P = S.Part2
	local Goal = { Transparency = 0.5 }
	for _ = 1, N do
		local Tween = TweenService:Create(P, Info, Goal)
		Tween:Play()
		Tween:Cancel()
	end
end)
W("tween", "Play.Cancel", 2000, function(N, S)
	local Tween = TweenService:Create(S.Part2, Info, { Transparency = 0.5 })
	for _ = 1, N do
		Tween:Play()
		Tween:Cancel()
	end
end)
W("tween", "GetValue.linear", 20000, function(N)
	for I = 1, N do
		local _ = TweenService:GetValue((I % 100) / 100, Enum.EasingStyle.Linear, Enum.EasingDirection.In)
	end
end)
W("tween", "GetValue.sine", 20000, function(N)
	for I = 1, N do
		local _ = TweenService:GetValue((I % 100) / 100, Enum.EasingStyle.Sine, Enum.EasingDirection.InOut)
	end
end)
W("tween", "GetValue", 20000, function(N)
	for I = 1, N do
		local _ = TweenService:GetValue((I % 100) / 100, Enum.EasingStyle.Quad, Enum.EasingDirection.Out)
	end
end)

-- datatype: constructors, operators and members
local v1, v2 = Vector3.new(1, 2, 3), Vector3.new(4, 5, 6)
local k1, k2 = Color3.new(0.2, 0.4, 0.6), Color3.new(0.8, 0.6, 0.4)
local u1, u2 = UDim2.new(0.5, 10, 0.25, 4), UDim2.new(0.1, 2, 0.3, 8)
local function datatype(Name, Fn, N)
	W("datatype", Name, N or 20000, Fn)
end
datatype("Vector3.new", function(N)
	for I = 1, N do
		local _ = Vector3.new(I, 2, 3)
	end
end)
datatype("Vector3.add", function(N)
	local a, b = v1, v2
	for _ = 1, N do
		local _ = a + b
	end
end)
datatype("Vector3.Magnitude", function(N)
	local a = v1
	for _ = 1, N do
		local _ = a.Magnitude
	end
end)
datatype("Vector3.Unit", function(N)
	local a = v1
	for _ = 1, N do
		local _ = a.Unit
	end
end)
datatype("Vector3.Dot", function(N)
	local a, b = v1, v2
	for _ = 1, N do
		local _ = a:Dot(b)
	end
end)
datatype("Vector3.Cross", function(N)
	local a, b = v1, v2
	for _ = 1, N do
		local _ = a:Cross(b)
	end
end)
datatype("Vector3.Lerp", function(N)
	local a, b = v1, v2
	for _ = 1, N do
		local _ = a:Lerp(b, 0.5)
	end
end)
datatype("CFrame.new3", function(N)
	for I = 1, N do
		local _ = CFrame.new(I, 2, 3)
	end
end)
datatype("CFrame.lookAt", function(N)
	local a, b = v1, v2
	for _ = 1, N do
		local _ = CFrame.lookAt(a, b)
	end
end)
datatype("CFrame.Angles", function(N)
	for I = 1, N do
		local _ = CFrame.Angles(I * 0.001, 0.2, 0.3)
	end
end)
datatype("CFrame.mul", function(N)
	local a, b = c1, c2
	for _ = 1, N do
		local _ = a * b
	end
end)
datatype("CFrame.mulVector3", function(N)
	local a, b = c1, v1
	for _ = 1, N do
		local _ = a * b
	end
end)
datatype("CFrame.Inverse", function(N)
	local a = c1
	for _ = 1, N do
		local _ = a:Inverse()
	end
end)
datatype("CFrame.Lerp", function(N)
	local a, b = c1, c2
	for _ = 1, N do
		local _ = a:Lerp(b, 0.5)
	end
end)
datatype("CFrame.ToObjectSpace", function(N)
	local a, b = c1, c2
	for _ = 1, N do
		local _ = a:ToObjectSpace(b)
	end
end)
datatype("CFrame.PointToObjectSpace", function(N)
	local a, b = c1, v1
	for _ = 1, N do
		local _ = a:PointToObjectSpace(b)
	end
end)
datatype("CFrame.Position", function(N)
	local a = c1
	for _ = 1, N do
		local _ = a.Position
	end
end)
datatype("CFrame.LookVector", function(N)
	local a = c1
	for _ = 1, N do
		local _ = a.LookVector
	end
end)
datatype("CFrame.GetComponents", function(N)
	local a = c1
	for _ = 1, N do
		local _ = a:GetComponents()
	end
end)
datatype("CFrame.ToEulerAnglesXYZ", function(N)
	local a = c1
	for _ = 1, N do
		local _ = a:ToEulerAnglesXYZ()
	end
end)
-- FuzzyEq: has a CFrame moved (a pair a micro-stud apart, the default eps: position and rotation compared), one that moved a stud
-- (a client answers from the position alone), and two that differ in both parts with an eps given
local c1near, c1moved = c1 * CFrame.new(1e-6, 0, 0), c1 + Vector3.new(1, 0, 0)
datatype("CFrame.FuzzyEq", function(N)
	local a, b = c1, c1near
	for _ = 1, N do
		local _ = a:FuzzyEq(b)
	end
end)
datatype("CFrame.FuzzyEq.false", function(N)
	local a, b = c1, c1moved
	for _ = 1, N do
		local _ = a:FuzzyEq(b)
	end
end)
datatype("CFrame.FuzzyEq.eps", function(N)
	local a, b = c1, c2
	for _ = 1, N do
		local _ = a:FuzzyEq(b, 0.01)
	end
end)
datatype("Color3.new", function(N)
	for I = 1, N do
		local _ = Color3.new(I / N, 0.5, 0.5)
	end
end)
datatype("Color3.fromRGB", function(N)
	for I = 1, N do
		local _ = Color3.fromRGB(I % 256, 128, 64)
	end
end)
datatype("Color3.fromHSV", function(N)
	for I = 1, N do
		local _ = Color3.fromHSV((I % 100) / 100, 0.5, 0.5)
	end
end)
datatype("Color3.Lerp", function(N)
	local a, b = k1, k2
	for _ = 1, N do
		local _ = a:Lerp(b, 0.5)
	end
end)
datatype("Color3.ToHSV", function(N)
	local a = k1
	for _ = 1, N do
		local _ = a:ToHSV()
	end
end)
datatype("Color3.R", function(N)
	local a = k1
	for _ = 1, N do
		local _ = a.R
	end
end)
datatype("UDim2.new", function(N)
	for I = 1, N do
		local _ = UDim2.new(0.5, I, 0.5, 0)
	end
end)
datatype("UDim2.fromOffset", function(N)
	for I = 1, N do
		local _ = UDim2.fromOffset(I, 4)
	end
end)
datatype("UDim2.fromScale", function(N)
	for I = 1, N do
		local _ = UDim2.fromScale(I / N, 0.5)
	end
end)
datatype("UDim2.add", function(N)
	local a, b = u1, u2
	for _ = 1, N do
		local _ = a + b
	end
end)
datatype("UDim2.Lerp", function(N)
	local a, b = u1, u2
	for _ = 1, N do
		local _ = a:Lerp(b, 0.5)
	end
end)
datatype("UDim2.X.Offset", function(N)
	local a = u1
	for _ = 1, N do
		local _ = a.X.Offset
	end
end)
datatype("Vector2.new", function(N)
	for I = 1, N do
		local _ = Vector2.new(I, 2)
	end
end)
datatype("Vector2.add", function(N)
	local a, b = Vector2.new(1, 2), Vector2.new(3, 4)
	for _ = 1, N do
		local _ = a + b
	end
end)
datatype("Vector2.Magnitude", function(N)
	local a = Vector2.new(3, 4)
	for _ = 1, N do
		local _ = a.Magnitude
	end
end)
datatype("NumberSequence.new2", function(N)
	for I = 1, N do
		local _ = NumberSequence.new(0, I / N)
	end
end, 10000)
datatype("NumberSequence.keypoints3", function(N)
	for I = 1, N do
		local _ = NumberSequence.new({ NumberSequenceKeypoint.new(0, 0), NumberSequenceKeypoint.new(0.5, I / N), NumberSequenceKeypoint.new(1, 1) })
	end
end, 5000)
datatype("ColorSequence.new2", function(N)
	local a, b = k1, k2
	for _ = 1, N do
		local _ = ColorSequence.new(a, b)
	end
end, 10000)
datatype("ColorSequence.keypoints3", function(N)
	local a, b = k1, k2
	for I = 1, N do
		local _ = ColorSequence.new({ ColorSequenceKeypoint.new(0, a), ColorSequenceKeypoint.new(I / (N + 1), b), ColorSequenceKeypoint.new(1, a) })
	end
end, 5000)
datatype("NumberRange.new", function(N)
	for I = 1, N do
		local _ = NumberRange.new(I, I + 1)
	end
end)
datatype("BrickColor.new", function(N)
	for _ = 1, N do
		local _ = BrickColor.new("Bright red")
	end
end, 10000)
datatype("Random.NextNumber", function(N)
	local R = Random.new(1)
	for _ = 1, N do
		local _ = R:NextNumber()
	end
end)
datatype("math.random", function(N)
	for _ = 1, N do
		local _ = math.random()
	end
end)
datatype("math.noise", function(N)
	for I = 1, N do
		local _ = math.noise(I * 0.1, 0.5)
	end
end)
datatype("typeof", function(N)
	local a = v1
	for _ = 1, N do
		local _ = typeof(a)
	end
end)
datatype("Enum.item", function(N)
	for _ = 1, N do
		local _ = Enum.Material.Neon
	end
end)

-- datatype, second pass: Vector2 and UDim2 members and arithmetic (a UI script's per-frame work), more CFrame members and the
-- sequence keypoints a gradient or an effect rebuilds
local w1, w2 = Vector2.new(3, 4), Vector2.new(1, 2)
local d1, d2 = UDim.new(0.5, 10), UDim.new(0.25, 4)
local Keys5 = {
	ColorSequenceKeypoint.new(0, k1), ColorSequenceKeypoint.new(0.25, k2), ColorSequenceKeypoint.new(0.5, k1),
	ColorSequenceKeypoint.new(0.75, k2), ColorSequenceKeypoint.new(1, k1),
}
local Sequence5 = ColorSequence.new(Keys5)
datatype("Vector2.X", function(N)
	local a = w1
	for _ = 1, N do
		local _ = a.X
	end
end)
datatype("Vector2.Y", function(N)
	local a = w1
	for _ = 1, N do
		local _ = a.Y
	end
end)
datatype("Vector2.x", function(N)
	local a = w1
	for _ = 1, N do
		local _ = a.x
	end
end)
datatype("Vector2.XY.sum", function(N)
	local a = w1
	for _ = 1, N do
		local _ = a.X + a.Y
	end
end)
datatype("Vector2.sub", function(N)
	local a, b = w1, w2
	for _ = 1, N do
		local _ = a - b
	end
end)
datatype("Vector2.mul", function(N)
	local a, b = w1, w2
	for _ = 1, N do
		local _ = a * b
	end
end)
datatype("Vector2.mul.number", function(N)
	local a = w1
	for _ = 1, N do
		local _ = a * 2
	end
end)
datatype("Vector2.div.number", function(N)
	local a = w1
	for _ = 1, N do
		local _ = a / 2
	end
end)
datatype("Vector2.unm", function(N)
	local a = w1
	for _ = 1, N do
		local _ = -a
	end
end)
datatype("Vector2.eq", function(N)
	local a, b = w1, w2
	for _ = 1, N do
		local _ = a == b
	end
end)
datatype("Vector2.Unit", function(N)
	local a = w1
	for _ = 1, N do
		local _ = a.Unit
	end
end)
datatype("Vector2.Dot", function(N)
	local a, b = w1, w2
	for _ = 1, N do
		local _ = a:Dot(b)
	end
end)
datatype("Vector2.Lerp", function(N)
	local a, b = w1, w2
	for _ = 1, N do
		local _ = a:Lerp(b, 0.5)
	end
end)
datatype("Vector2.Cross", function(N)
	local a, b = w1, w2
	for _ = 1, N do
		local _ = a:Cross(b)
	end
end)
datatype("UDim2.X", function(N)
	local a = u1
	for _ = 1, N do
		local _ = a.X
	end
end)
datatype("UDim2.Width", function(N)
	local a = u1
	for _ = 1, N do
		local _ = a.Width
	end
end)
datatype("UDim2.Y.Scale", function(N)
	local a = u1
	for _ = 1, N do
		local _ = a.Y.Scale
	end
end)
datatype("UDim2.sub", function(N)
	local a, b = u1, u2
	for _ = 1, N do
		local _ = a - b
	end
end)
datatype("UDim2.eq", function(N)
	local a, b = u1, u2
	for _ = 1, N do
		local _ = a == b
	end
end)
datatype("UDim.new", function(N)
	for I = 1, N do
		local _ = UDim.new(0.5, I)
	end
end)
datatype("UDim.add", function(N)
	local a, b = d1, d2
	for _ = 1, N do
		local _ = a + b
	end
end)
datatype("UDim.Scale", function(N)
	local a = d1
	for _ = 1, N do
		local _ = a.Scale
	end
end)
datatype("Vector3.X", function(N)
	local a = v1
	for _ = 1, N do
		local _ = a.X
	end
end)
datatype("CFrame.X", function(N)
	local a = c1
	for _ = 1, N do
		local _ = a.X
	end
end)
datatype("CFrame.Rotation", function(N)
	local a = c1
	for _ = 1, N do
		local _ = a.Rotation
	end
end)
datatype("CFrame.RightVector", function(N)
	local a = c1
	for _ = 1, N do
		local _ = a.RightVector
	end
end)
datatype("CFrame.new12", function(N)
	for I = 1, N do
		local _ = CFrame.new(I, 2, 3, 1, 0, 0, 0, 1, 0, 0, 0, 1)
	end
end)
datatype("CFrame.add", function(N)
	local a, b = c1, v1
	for _ = 1, N do
		local _ = a + b
	end
end)
datatype("CFrame.GetComponents.all", function(N)
	local a = c1
	for _ = 1, N do
		local _x, _y, _z, _r00, _r01, _r02, _r10, _r11, _r12, _r20, _r21, _r22 = a:GetComponents()
	end
end)
datatype("CFrame.ToOrientation", function(N)
	local a = c1
	for _ = 1, N do
		local _ = a:ToOrientation()
	end
end)
datatype("ColorSequenceKeypoint.new", function(N)
	local a = k1
	for I = 1, N do
		local _ = ColorSequenceKeypoint.new(I / (N + 1), a)
	end
end)
datatype("NumberSequenceKeypoint.new", function(N)
	for I = 1, N do
		local _ = NumberSequenceKeypoint.new(I / (N + 1), 0.5)
	end
end)
datatype("ColorSequence.keypoints5", function(N)
	local Keys = Keys5
	for _ = 1, N do
		local _ = ColorSequence.new(Keys)
	end
end, 10000)
datatype("ColorSequence.new1", function(N)
	local a = k1
	for _ = 1, N do
		local _ = ColorSequence.new(a)
	end
end, 10000)
datatype("NumberSequence.new1", function(N)
	for I = 1, N do
		local _ = NumberSequence.new(I / N)
	end
end, 10000)
datatype("ColorSequence.Keypoints", function(N)
	local a = Sequence5
	for _ = 1, N do
		local _ = a.Keypoints
	end
end, 10000)
datatype("ColorSequence.Keypoints2", function(N)
	local a = ColorSequence.new(k1, k2)
	for _ = 1, N do
		local _ = a.Keypoints
	end
end, 10000)
datatype("NumberSequence.Keypoints", function(N)
	local a = NumberSequence.new({ NumberSequenceKeypoint.new(0, 0), NumberSequenceKeypoint.new(0.25, 1), NumberSequenceKeypoint.new(0.5, 0),
		NumberSequenceKeypoint.new(0.75, 1), NumberSequenceKeypoint.new(1, 0) })
	for _ = 1, N do
		local _ = a.Keypoints
	end
end, 10000)
datatype("NumberSequence.Keypoints2", function(N)
	local a = NumberSequence.new(0, 1)
	for _ = 1, N do
		local _ = a.Keypoints
	end
end, 10000)
datatype("ColorSequenceKeypoint.Value", function(N)
	local a = Keys5[2]
	for _ = 1, N do
		local _ = a.Value
	end
end)
datatype("NumberRange.Min", function(N)
	local a = NumberRange.new(1, 2)
	for _ = 1, N do
		local _ = a.Min
	end
end)
datatype("Rect.new", function(N)
	for I = 1, N do
		local _ = Rect.new(0, 0, I, 10)
	end
end)
datatype("TweenInfo.new", function(N)
	for I = 1, N do
		local _ = TweenInfo.new(I / N, Enum.EasingStyle.Quad)
	end
end, 10000)
datatype("Ray.new", function(N)
	local a, b = v1, v2
	for _ = 1, N do
		local _ = Ray.new(a, b)
	end
end)
datatype("Random.NextInteger", function(N)
	local R = Random.new(1)
	for _ = 1, N do
		local _ = R:NextInteger(1, 100)
	end
end)
datatype("Color3.eq", function(N)
	local a, b = k1, k2
	for _ = 1, N do
		local _ = a == b
	end
end)

-- lib: string and table library calls from a game module
local function lib(Name, N, Fn)
	W("lib", Name, N, Fn)
end
lib("string.format", 20000, function(N)
	for I = 1, N do
		local _ = string.format("%d/%d %s", I + Fresh, 100, "x")
	end
end)
lib("string.format.float", 20000, function(N)
	for I = 1, N do
		local _ = string.format("%.2f", (I + Fresh) * 0.37)
	end
end)
lib("tostring.number", 20000, function(N)
	for I = 1, N do
		local _ = tostring(I + Fresh)
	end
end)
lib("tonumber.string", 20000, function(N)
	for _ = 1, N do
		local _ = tonumber("12345")
	end
end)
lib("concat.number", 20000, function(N)
	for I = 1, N do
		local _ = "Wave " .. I + Fresh
	end
end)
lib("interpolate", 20000, function(N)
	for I = 1, N do
		local _ = `{I + Fresh}/{100}`
	end
end)
lib("string.sub", 20000, function(N)
	local S = "the quick brown fox"
	for I = 1, N do
		local _ = string.sub(S, I % 10 + 1, 15)
	end
end)
lib("string.find.plain", 20000, function(N)
	local S = "the quick brown fox jumps"
	for _ = 1, N do
		local _ = string.find(S, "fox", 1, true)
	end
end)
lib("string.match", 10000, function(N)
	local S = "Unit_42_Level"
	for _ = 1, N do
		local _ = string.match(S, "_(%d+)_")
	end
end)
lib("string.split", 10000, function(N)
	local S = "a,b,c,d,e"
	for _ = 1, N do
		local _ = string.split(S, ",")
	end
end)
lib("string.lower", 20000, function(N)
	local S = "Hello World"
	for _ = 1, N do
		local _ = string.lower(S)
	end
end)
lib("string.rep", 20000, function(N)
	for _ = 1, N do
		local _ = string.rep("ab", 8)
	end
end)
lib("table.insert", 20000, function(N)
	local T = {}
	for I = 1, N do
		table.insert(T, I)
	end
end)
lib("table.remove.last", 20000, function(N)
	local T = table.create(N, 1)
	for _ = 1, N do
		table.remove(T)
	end
end)
lib("table.remove.first16", 20000, function(N)
	local T = table.create(16, 1)
	for I = 1, N do
		table.remove(T, 1)
		T[16] = I
	end
end)
lib("table.find16", 20000, function(N)
	local T = {}
	for I = 1, 16 do
		T[I] = I
	end
	for _ = 1, N do
		local _ = table.find(T, 12)
	end
end)
lib("table.sort100", 200, function(N)
	local Base = {}
	for I = 1, 100 do
		Base[I] = (I * 37) % 101
	end
	for _ = 1, N do
		local T = table.clone(Base)
		table.sort(T)
	end
end)
lib("table.sort100.comparator", 200, function(N)
	local Base = {}
	for I = 1, 100 do
		Base[I] = (I * 37) % 101
	end
	local function less(A, B)
		return A > B
	end
	for _ = 1, N do
		local T = table.clone(Base)
		table.sort(T, less)
	end
end)
lib("table.sort50.comparator", 400, function(N)
	local Base = {}
	for I = 1, 50 do
		Base[I] = (I * 37) % 51
	end
	local function less(A, B)
		return A > B
	end
	for _ = 1, N do
		local T = table.clone(Base)
		table.sort(T, less)
	end
end)
lib("table.clone.array10", 20000, function(N)
	local T = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10 }
	for _ = 1, N do
		local _ = table.clone(T)
	end
end)
lib("table.concat20", 10000, function(N)
	local T = {}
	for I = 1, 20 do
		T[I] = "w" .. I
	end
	for _ = 1, N do
		local _ = table.concat(T, " ")
	end
end)
lib("table.concat10", 20000, function(N)
	local T = { "a", "b", "c", "d", "e", "f", "g", "h", "i", "j" }
	for _ = 1, N do
		local _ = table.concat(T, ",")
	end
end)
lib("table.clone10", 20000, function(N)
	local T = { a = 1, b = 2, c = 3, d = 4, e = 5, f = 6, g = 7, h = 8, i = 9, j = 10 }
	for _ = 1, N do
		local _ = table.clone(T)
	end
end)
lib("table.create100", 20000, function(N)
	for _ = 1, N do
		local _ = table.create(100, 0)
	end
end)
lib("table.unpack3", 20000, function(N)
	local T = { 1, 2, 3 }
	for _ = 1, N do
		local _, _, _ = table.unpack(T)
	end
end)
lib("JSONEncode.small", 2000, function(N)
	local T = { a = 1, b = "two", c = { 1, 2, 3 } }
	for _ = 1, N do
		local _ = HttpService:JSONEncode(T)
	end
end)
lib("JSONDecode.small", 2000, function(N)
	local Text = '{"a":1,"b":"two","c":[1,2,3]}'
	for _ = 1, N do
		local _ = HttpService:JSONDecode(Text)
	end
end)

-- libsize: library calls whose work grows with their input, each at several sizes (an operation is one call, or one piece of a
-- string built up): the work clock prices the elements they scan or move and the bytes they make (src/cost.rs LUAU_SCANNED and
-- below), where a flat price per call read a 100,000-element table.find as a 10-element one.
local KB, MB = 1024, 1024 * 1024
local function sizeLabel(Size)
	return if Size >= MB and Size % MB == 0 then `{Size // MB}m` elseif Size >= KB and Size % KB == 0 then `{Size // KB}k` else tostring(Size)
end
-- Rows `libsize.<Name>.<size>` for each size: `Ops(Size)` operations a round, `Fn(N, Size, State)`, `Prepare(N, Size)` -> State
local function sized(Name, Sizes, Ops, Fn, Prepare)
	for _, Size in Sizes do
		W("libsize", Name .. "." .. sizeLabel(Size), Ops(Size), function(N, _, State)
			return Fn(N, Size, State)
		end, if Prepare
			then {
				Prepare = function(N)
					return Prepare(N, Size)
				end,
			}
			else nil)
	end
end
-- An 8-character token no earlier call gave (hex digits of a counter): what a row makes from it is a new string, never one an earlier
-- round left alive, which Luau's interning would hand back without making it (and a client's collector may have swept already)
local Serial = 0
local function token()
	Serial += 1
	return string.format("%08x", Serial)
end
-- `Count` distinct strings of `Bytes` bytes: a token, then "abcdefgh" repeated (or `Body`)
local function subjects(Count, Bytes, _, Body)
	local Rest = string.rep(Body or "abcdefgh", (Bytes - 8) // 8)
	local Out = table.create(Count)
	for I = 1, Count do
		Out[I] = token() .. Rest
	end
	return Out
end
-- A string of `Bytes` bytes made of tokens only: every slice of it is a string no earlier round made
local function tokens(Bytes)
	local Parts = table.create(Bytes // 8)
	for I = 1, Bytes // 8 do
		Parts[I] = token()
	end
	return table.concat(Parts)
end
local function byBytes(Small, Medium, Large)
	return function(Size)
		return if Size <= KB then Small elseif Size <= 64 * KB then Medium else Large
	end
end
local Piece = "abcdefgh"
-- a string built from n pieces: `..` makes a new string of the whole each time (quadratic in n), table.concat joins once
sized("concat.build", { 250, 1000, 4000, 16000 }, function(Size)
	return Size
end, function(N)
	local S = token()
	for _ = 1, N do
		S ..= Piece
	end
	return nil
end)
sized("table.concat.build", { 250, 1000, 4000, 16000 }, function(Size)
	return Size
end, function(N)
	local T = table.create(N)
	T[1] = token()
	for I = 2, N do
		T[I] = Piece
	end
	local _ = table.concat(T)
end)
-- a list's last element: table.find compares every element before it
sized("table.find", { 10, 100, 1000, 10000, 100000 }, function(Size)
	return math.max(10, 200000 // Size)
end, function(N, Size, T)
	for _ = 1, N do
		local _ = table.find(T, Size)
	end
end, function(_, Size)
	local T = table.create(Size)
	for I = 1, Size do
		T[I] = I
	end
	return T
end)
-- draining a list from its front (each removal moves the rest down) and building one at its front
sized("table.remove.first", { 100, 1000, 10000 }, function(Size)
	return Size
end, function(N, _, T)
	for _ = 1, N do
		table.remove(T, 1)
	end
end, function(_, Size)
	return table.create(Size, 1)
end)
sized("table.insert.first", { 100, 1000, 10000 }, function(Size)
	return Size
end, function(N)
	local T = {}
	for I = 1, N do
		table.insert(T, 1, I)
	end
end)
sized("table.move", { 16, 1000, 100000 }, function(Size)
	return math.max(10, 200000 // Size)
end, function(N, Size, State)
	for _ = 1, N do
		table.move(State.A, 1, Size, 1, State.B)
	end
end, function(_, Size)
	return { A = table.create(Size, 7), B = table.create(Size, 0) }
end)
sized("table.unpack", { 3, 30, 300 }, function(Size)
	return math.max(1000, 60000 // Size)
end, function(N, _, T)
	for _ = 1, N do
		local _ = select("#", table.unpack(T))
	end
end, function(_, Size)
	return table.create(Size, 1)
end)
local Bytes = { KB, 64 * KB, MB }
sized("string.rep", Bytes, byBytes(5000, 200, 12), function(N, Size, Pieces)
	for I = 1, N do
		local _ = string.rep(Pieces[I], Size // 8)
	end
end, function(N)
	local Pieces = table.create(N)
	for I = 1, N do
		Pieces[I] = token()
	end
	return Pieces
end)
sized("string.sub", Bytes, byBytes(5000, 200, 12), function(N, Size, Subject)
	for I = 1, N do
		local At = (I + Fresh) % 8192 + 1
		local _ = string.sub(Subject, At, At + Size - 1)
	end
end, function(_, Size)
	return tokens(Size + 8192)
end)
sized("string.upper", Bytes, byBytes(5000, 200, 12), function(N, _, List)
	for I = 1, N do
		local _ = string.upper(List[I])
	end
end, function(N, Size)
	return subjects(N, Size, Fresh)
end)
sized("string.lower", Bytes, byBytes(5000, 200, 12), function(N, _, List)
	for I = 1, N do
		local _ = string.lower(List[I])
	end
end, function(N, Size)
	return subjects(N, Size, Fresh, "ABCDEFGH")
end)
-- every 8th byte replaced
sized("string.gsub", Bytes, byBytes(2000, 40, 3), function(N, _, List)
	for I = 1, N do
		local _ = string.gsub(List[I], "a", "z")
	end
end, function(N, Size)
	return subjects(N, Size, Fresh)
end)
-- searches that find nothing scan the whole subject
sized("string.find.plain", Bytes, byBytes(20000, 500, 30), function(N, _, Subject)
	for _ = 1, N do
		local _ = string.find(Subject, "zz", 1, true)
	end
end, function(_, Size)
	return subjects(1, Size, 0)[1]
end)
sized("string.find.pattern", Bytes, byBytes(5000, 100, 6), function(N, _, Subject)
	for _ = 1, N do
		local _ = string.find(Subject, "%d%d%d%d%d%d%d%d%d")
	end
end, function(_, Size)
	return subjects(1, Size, 0)[1]
end)
sized("string.match", Bytes, byBytes(5000, 100, 6), function(N, _, Subject)
	for _ = 1, N do
		local _ = string.match(Subject, "(%d+)$")
	end
end, function(_, Size)
	return subjects(1, Size, 0)[1]
end)
-- words of 7 digits and a space, all different: each match makes a new string
sized("string.gmatch", Bytes, byBytes(200, 4, 1), function(N, _, List)
	for I = 1, N do
		local Count = 0
		for _ in string.gmatch(List[I], "%S+") do
			Count += 1
		end
	end
end, function(N, Size)
	local Words = Size // 8
	local List = table.create(N)
	for I = 1, N do
		local Parts = table.create(Words)
		for J = 1, Words do
			Serial += 1
			Parts[J] = string.format("%07x ", Serial % 0x10000000)
		end
		List[I] = table.concat(Parts)
	end
	return List
end)
sized("string.byte", { 16, 256, 4096 }, function(Size)
	return math.max(200, 400000 // Size)
end, function(N, Size, Subject)
	for _ = 1, N do
		local _ = select("#", string.byte(Subject, 1, Size))
	end
end, function(_, Size)
	return subjects(1, math.max(Size, 16), 0)[1]
end)
sized("string.char", { 16, 256, 4096 }, function(Size)
	return math.max(200, 400000 // Size)
end, function(N, Size, Codes)
	for I = 1, N do
		Codes[1] = 48 + (I + Fresh) % 10
		local _ = string.char(table.unpack(Codes, 1, Size))
	end
end, function(_, Size)
	local Codes = table.create(Size)
	for I = 1, Size do
		Codes[I] = 97 + I % 26
	end
	return Codes
end)
sized("string.format.long", Bytes, byBytes(5000, 200, 12), function(N, _, Subject)
	for I = 1, N do
		local _ = string.format("%s|%d", Subject, I + Fresh)
	end
end, function(_, Size)
	return subjects(1, Size, 0)[1]
end)
-- (buffers at 256 KB and 4 MB too: a large one's memory is fresh, and a large copy streams past a core's nearest caches)
local BufferBytes = { KB, 64 * KB, 256 * KB, MB, 4 * MB }
local function byBuffer(Small, Medium, Large)
	return function(Size)
		return if Size <= KB then Small elseif Size <= 64 * KB then Medium elseif Size <= MB then Large else math.max(2, Large // 4)
	end
end
sized("buffer.create", BufferBytes, byBuffer(5000, 200, 12), function(N, Size)
	for _ = 1, N do
		local _ = buffer.create(Size)
	end
end)
sized("buffer.fill", BufferBytes, byBuffer(20000, 500, 30), function(N, _, B)
	for I = 1, N do
		buffer.fill(B, 0, I % 256)
	end
end, function(_, Size)
	return buffer.create(Size)
end)
sized("buffer.copy", BufferBytes, byBuffer(20000, 500, 30), function(N, _, State)
	for _ = 1, N do
		buffer.copy(State.To, 0, State.From)
	end
end, function(_, Size)
	return { To = buffer.create(Size), From = buffer.create(Size) }
end)
sized("buffer.tostring", Bytes, byBytes(5000, 200, 12), function(N, _, B)
	for I = 1, N do
		buffer.writeu32(B, 0, I + Fresh)
		local _ = buffer.tostring(B)
	end
end, function(_, Size)
	return buffer.create(Size)
end)
sized("buffer.writestring", BufferBytes, byBuffer(20000, 500, 30), function(N, _, State)
	for _ = 1, N do
		buffer.writestring(State.B, 0, State.S)
	end
end, function(_, Size)
	return { B = buffer.create(Size), S = subjects(1, Size, 0)[1] }
end)
sized("utf8.len", Bytes, byBytes(20000, 500, 30), function(N, _, Subject)
	for _ = 1, N do
		local _ = utf8.len(Subject)
	end
end, function(_, Size)
	return subjects(1, Size, 0)[1]
end)
sized("utf8.codes", Bytes, byBytes(500, 10, 1), function(N, _, Subject)
	for _ = 1, N do
		for _, _ in utf8.codes(Subject) do
		end
	end
end, function(_, Size)
	return subjects(1, Size, 0)[1]
end)
sized("utf8.char", { 16, 256, 4096 }, function(Size)
	return math.max(200, 400000 // Size)
end, function(N, Size, Codes)
	for I = 1, N do
		Codes[1] = 0x4E00 + (I + Fresh) % 4096
		local _ = utf8.char(table.unpack(Codes, 1, Size))
	end
end, function(_, Size)
	local Codes = table.create(Size)
	for I = 1, Size do
		Codes[I] = 0x4E00 + I % 512
	end
	return Codes
end)

-- tables grown one key at a time from empty, as a list and as string keys, at several final sizes (an operation is a key: the
-- growth's rehashes and the storage it allocates, src/cost.rs LUAU_REHASHED and LUAU_STORED), and tables made at their size
local function builds(Size)
	return math.max(1, 20000 // Size)
end
sized("grow.list", { 16, 1000, 100000 }, function(Size)
	return builds(Size) * Size
end, function(_, Size)
	for _ = 1, builds(Size) do
		local T = {}
		for I = 1, Size do
			T[I] = I
		end
	end
end)
sized("grow.keys", { 16, 1000, 100000 }, function(Size)
	return builds(Size) * Size
end, function(_, Size, Keys)
	for _ = 1, builds(Size) do
		local T = {}
		for I = 1, Size do
			T[Keys[I]] = I
		end
	end
end, function(_, Size)
	local Keys = table.create(Size)
	for I = 1, Size do
		Keys[I] = "k" .. I
	end
	return Keys
end)
-- a dictionary grown one string key at a time, as records and lookup tables grow (keys reinserted into the hash part as it grows)
sized("grow.dict", { 32, 256, 4096 }, function(Size)
	return builds(Size) * Size
end, function(_, Size, Keys)
	for _ = 1, builds(Size) do
		local T = {}
		for I = 1, Size do
			T[Keys[I]] = true
		end
	end
end, function(_, Size)
	local Keys = table.create(Size)
	for I = 1, Size do
		Keys[I] = "d" .. I
	end
	return Keys
end)
sized("table.create", { 4, 64, 1024, 16384 }, function(Size)
	return math.max(20, 400000 // Size)
end, function(N, Size)
	for _ = 1, N do
		local _ = table.create(Size)
	end
end)

-- json: HttpService's JSON at several sizes, a list of records (about 105 bytes and 9 values a record)
local function records(Count)
	local List = table.create(Count)
	for I = 1, Count do
		List[I] = { id = 100000 + I, name = "unit" .. I, level = I % 60, tags = { "fire", "aoe" }, stats = { damage = I * 1.5, range = 20 } }
	end
	return List
end
for _, Count in { 10, 100, 1000, 10000 } do
	local Ops = math.max(1, 5000 // Count)
	W("json", "JSONEncode.records" .. Count, Ops, function(N, _, List)
		for _ = 1, N do
			local _ = HttpService:JSONEncode(List)
		end
	end, {
		Prepare = function()
			return records(Count)
		end,
	})
	W("json", "JSONDecode.records" .. Count, Ops, function(N, _, Text)
		for _ = 1, N do
			local _ = HttpService:JSONDecode(Text)
		end
	end, {
		Prepare = function()
			return HttpService:JSONEncode(records(Count))
		end,
	})
end

-- gc: allocation with the collector running: short-lived objects on a small heap, then on a heap of about 50 MB
local function churn(N)
	local Keep
	for I = 1, N do
		Keep = { I, I + 1, I + 2, I + 3 }
	end
	return Keep
end
W("gc", "churn.tables.small_heap", 200000, function(N)
	churn(N)
end)
W("gc", "churn.strings", 100000, function(N)
	local Keep
	for I = 1, N do
		Keep = "item" .. I + Fresh
	end
	return Keep
end)
W("gc", "churn.closures", 200000, function(N)
	local Keep
	for I = 1, N do
		Keep = function()
			return I
		end
	end
	return Keep
end)
local BigHeap
W("gc", "churn.tables.big_heap", 200000, function(N)
	churn(N)
end, {
	Prepare = function()
		if not BigHeap then
			BigHeap = table.create(400000)
			for I = 1, 400000 do
				BigHeap[I] = { I, I + 1, I + 2, I + 3 }
			end
		end
	end,
	Finish = function()
		BigHeap = nil
	end,
})
-- the collector's share of an allocated byte: 1 KB objects made and dropped on a small heap and on the heap of about 50 MB above
-- (the difference is what marking a game-sized heap adds: little, src/cost.rs LUAU_COLLECTOR_OBJECT)
local function bigHeap()
	if not BigHeap then
		BigHeap = table.create(400000)
		for I = 1, 400000 do
			BigHeap[I] = { I, I + 1, I + 2, I + 3 }
		end
	end
end
local function dropHeap()
	BigHeap = nil
end
local KiloBody = string.rep("abcdefgh", 127)
for _, Heap in { "small_heap", "big_heap" } do
	local Options = if Heap == "big_heap" then { Prepare = bigHeap, Finish = dropHeap } else nil
	W("gc", "churn.buffers1k." .. Heap, 50000, function(N)
		local Keep
		for _ = 1, N do
			Keep = buffer.create(1024)
		end
		return Keep
	end, Options)
	W("gc", "churn.tables128." .. Heap, 50000, function(N)
		local Keep
		for _ = 1, N do
			Keep = table.create(128)
		end
		return Keep
	end, Options)
	W("gc", "churn.strings1k." .. Heap, 50000, function(N, _, Heads)
		local Keep
		for I = 1, N do
			Keep = Heads[I] .. KiloBody
		end
		return Keep
	end, {
		Prepare = function(N)
			if Options then
				bigHeap()
			end
			local Heads = table.create(N)
			for I = 1, N do
				Heads[I] = token()
			end
			return Heads
		end,
		Finish = Options and dropHeap,
	})
end

-- mem: bytes an item kept (collectgarbage("count") before and after building them). Every round's items stay alive until the row
-- ends, so no round's garbage is there for the collector to free while a later round builds; the row reads its median round, which a
-- round that grew a table the engine keeps for itself (its cache of instance references doubling) does not move.
local function memory(Name, Count, Build)
	W("mem", Name, Count, function(N)
		local Before = collectgarbage("count")
		local Kept = Build(N)
		local After = collectgarbage("count")
		Corpus.Keep = Corpus.Keep or {}
		table.insert(Corpus.Keep, Kept)
		return (After - Before) * 1024
	end, {
		Memory = true,
		Finish = function()
			Corpus.Keep = nil
		end,
	})
end
memory("table.array3", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = { I, I, I }
	end
	return T
end)
memory("table.record4", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = { x = I, y = I, z = I, w = I }
	end
	return T
end)
memory("table.empty", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = {}
	end
	return T
end)
memory("string.distinct24", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = string.format("item-%018d", I)
	end
	return T
end)
memory("closure.upvalue1", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = function()
			return I
		end
	end
	return T
end)
memory("array.numbers", 100000, function(N)
	return table.create(N, 1.5)
end)
memory("Vector3", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = Vector3.new(I, 0, 0)
	end
	return T
end)
memory("CFrame", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = CFrame.new(I, 0, 0)
	end
	return T
end)
memory("Color3", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = Color3.new(I / N, 0, 0)
	end
	return T
end)
memory("UDim2", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = UDim2.fromOffset(I, 0)
	end
	return T
end)
memory("buffer100", 10000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = buffer.create(100)
	end
	return T
end)
memory("Instance.Part", 2000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = Instance.new("Part")
	end
	return T
end)
memory("Instance.Folder", 2000, function(N)
	local T = table.create(N)
	for I = 1, N do
		T[I] = Instance.new("Folder")
	end
	return T
end)
-- what the heap holds for an instance: by class, parented, and for a clone whose ten children scripts have not touched (one
-- reference made, eleven instances) against the same clones once GetChildren handed scripts all eleven
for _, Class in { "NumberValue", "Frame", "Model", "Attachment", "UIGradient" } do
	memory("Instance." .. Class, 2000, function(N)
		local T = table.create(N)
		for I = 1, N do
			T[I] = Instance.new(Class)
		end
		return T
	end)
end
memory("Instance.Folder.parented", 2000, function(N)
	local Holder = Instance.new("Folder")
	local T = table.create(N)
	for I = 1, N do
		local F = Instance.new("Folder")
		F.Parent = Holder
		T[I] = F
	end
	return { T, Holder }
end)
local CloneTemplate
local function cloneTemplate()
	if not CloneTemplate then
		CloneTemplate = Instance.new("Folder")
		for I = 1, 10 do
			local C = Instance.new("Folder")
			C.Name = "C" .. I
			C.Parent = CloneTemplate
		end
	end
	return CloneTemplate
end
cloneTemplate()
memory("Instance.Folder.clone11", 500, function(N)
	local Template = CloneTemplate
	local T = table.create(N)
	for I = 1, N do
		T[I] = Template:Clone()
	end
	return T
end)
memory("Instance.Folder.clone11.touched", 500, function(N)
	local Template = CloneTemplate
	local T = table.create(N)
	for I = 1, N do
		local Copy = Template:Clone()
		T[I] = { Copy, Copy:GetChildren() }
	end
	return T
end)

------------------------------------------------------------------------------------------------ the runner

-- Language.luau's workloads, interpreted and native, as rows of the `lang` family (`lang.<name>` and `native.<name>`)
local function languageRows(Language)
	local Rows = {}
	for _, Variant in { "interp", "native", "unsafe" } do
		local Module = Language[Variant]
		if Module then
			-- (the unsafe module, whose environment getfenv deoptimized, runs only the rows it names)
			local Prefix = if Variant == "interp" then "lang." elseif Variant == "native" then "native." else "lang.unsafe."
			for _, Name in (if Variant == "unsafe" then Module.Unsafe else Module.Order) do
				local Entry = Module.W[Name]
				table.insert(Rows, { Family = if Variant == "native" then "native" else "lang", Name = Prefix .. Name, N = Entry[1], Fn = function(N)
					Entry[2](N)
				end })
			end
		end
	end
	return Rows
end

-- ree's VM work counters across one call (ree with REE_WORK_COUNTERS=1 only; the engine has no os.workcounters): what a
-- round did, for fitting prices to the engine's times (tools/cost-parity/fit.py)
local Counters = (os :: any).workcounters
local CounterKeys = { "allocated", "ccalls", "tmcalls", "hostcalls", "objects", "stored", "rehashed", "hostnanos", "vectormagnitudes", "vectorunits", "vectormethods",
	"numtostr", "sortcompares", "sortcalls", "createitems", "cloneitems", "concatitems", "splitparts", "formats", "formatspecs", "newstrings",
	"slowimports", "coldtables", "coldclosures", "coldprotos", "coldcode", "scanned", "moved", "strbytes", "strscanned", "matchsteps",
	"bufbytes", "allocbytes", "libobjects", "casebytes", "utf8bytes", "utf8chars", "strchars", "bytevalues", "buffers", "matchclasses",
	"hashrehashed", "largeblocks", "buffreshbytes", "bufbigcopybytes", "coldcfuncs", "coldhostfuncs", "rareprotos", "jsonbytes", "jsonvalues" }
local function counted(Fn, ...)
	local A = Counters()
	Fn(...)
	local B = Counters()
	local Out = { ops = {} }
	for Op, Count in B.ops do
		local D = Count - (A.ops[Op] or 0)
		if D > 0 then
			Out.ops[tostring(Op - 1)] = D
		end
	end
	if B.nativeops then
		Out.nativeops = {}
		for Op, Count in B.nativeops do
			local D = Count - (A.nativeops[Op] or 0)
			if D > 0 then
				Out.nativeops[tostring(Op - 1)] = D
			end
		end
	end
	for _, Key in CounterKeys do
		if B[Key] then
			Out[Key] = B[Key] - A[Key]
		end
	end
	return Out
end

local function measure(Entry, S, Rounds)
	local N = Entry.N
	local Warm = if Entry.Measured or Entry.Memory then N else math.max(1, math.min(N, 50))
	local State = if Entry.Prepare then Entry.Prepare(Warm, S) else nil
	Entry.Fn(Warm, S, State)
	if Entry.Cleanup then
		Entry.Cleanup(State, S)
	end
	local Samples = {}
	for Round = 1, Rounds do
		task.wait()
		Fresh += 1000000
		State = if Entry.Prepare then Entry.Prepare(N, S) else nil
		local T0 = os.clock()
		local Own = Entry.Fn(N, S, State)
		local T1 = os.clock()
		if Entry.Cleanup then
			Entry.Cleanup(State, S)
		end
		local Seconds = if Entry.Measured or Entry.Memory then Own else T1 - T0
		Samples[Round] = if Entry.Memory then Seconds / N else Seconds / N * 1e9
	end
	if Entry.Finish then
		Entry.Finish()
	end
	local Work = nil
	if Counters and not Entry.Memory and not Entry.Measured then
		task.wait()
		Fresh += 1000000
		State = if Entry.Prepare then Entry.Prepare(N, S) else nil
		Work = counted(Entry.Fn, N, S, State)
		if Entry.Cleanup then
			Entry.Cleanup(State, S)
		end
		if Entry.Finish then
			Entry.Finish()
		end
	end
	return Samples, Work
end

-- Options: Families (a list; nil: every family), Rounds (default 9), Language ({ interp = module, native = module }),
-- GuiParent (where the GUI goes; default the local player's PlayerGui), Print (a line printer for progress).
function Corpus.Run(Options)
	Options = Options or {}
	local Rounds = Options.Rounds or 9
	local Wanted = nil
	if Options.Families then
		Wanted = {}
		for _, Family in Options.Families do
			Wanted[Family] = true
		end
	end
	local Entries = {}
	for _, Entry in List do
		if Wanted == nil or Wanted[Entry.Family] then
			table.insert(Entries, Entry)
		end
	end
	if Options.Language and (Wanted == nil or Wanted.lang or Wanted.native) then
		for _, Entry in languageRows(Options.Language) do
			if Wanted == nil or Wanted[Entry.Family] then
				table.insert(Entries, Entry)
			end
		end
	end
	local S = scene(Options)
	local Rows = {}
	for _, Entry in Entries do
		local Ok, Samples, Work = pcall(measure, Entry, S, Rounds)
		local Row = { name = Entry.Name, family = Entry.Family, n = Entry.N }
		if Ok then
			Row.samples = Samples
			Row.median = median(Samples)
			Row.min = math.min(table.unpack(Samples))
			Row.max = math.max(table.unpack(Samples))
			Row.unit = if Entry.Memory then "bytes" else "ns"
			Row.value = Row.median
			Row.work = Work
		else
			Row.error = tostring(Samples)
		end
		table.insert(Rows, Row)
		if Options.Print then
			Options.Print(if Ok then string.format("%-48s %10.2f %s", Row.name, Row.median, Row.unit) else Row.name .. " ERROR " .. Row.error)
		end
	end
	local Meta = {
		version = version(),
		placeId = game.PlaceId,
		studio = RunService:IsStudio(),
		rounds = Rounds,
		heapKB = collectgarbage("count"),
	}
	local Read, Behavior = pcall(function()
		return workspace.SignalBehavior.Name
	end)
	Meta.signalBehavior = if Read then Behavior else "unreadable"
	Meta.localizationTables = #game:GetService("LocalizationService"):GetChildren()
	-- (a constant vector's typeof, which a compiler may fold)
	Meta.typeofVector = typeof(Vector3.new(1, 2, 3))
	-- what the method rows' spatial queries find on this side
	local function found(F)
		local Ok, Value = pcall(F)
		return if Ok then Value else tostring(Value)
	end
	local function ray(From, Params)
		local Hit = workspace:Raycast(From, Vector3.new(0, -20, 0), Params)
		return if Hit then Hit.Instance.Name else "miss"
	end
	local Spatial = spatial(S)
	local Include = RaycastParams.new()
	Include.FilterType = Enum.RaycastFilterType.Include
	Include.FilterDescendantsInstances = { Spatial.Stack[1] }
	local Exclude = RaycastParams.new()
	Exclude.FilterType = Enum.RaycastFilterType.Exclude
	Exclude.FilterDescendantsInstances = { Spatial.Line }
	Meta.queries = {
		boxStack = found(function()
			return #workspace:GetPartBoundsInBox(CFrame.new(SpatialAt), Vector3.new(4, 4, 4), OverlapParams.new())
		end),
		boxLine = found(function()
			return #workspace:GetPartBoundsInBox(CFrame.new(SpatialAt + Vector3.new(16.5, 0, 20)), Vector3.new(30, 2, 2), OverlapParams.new())
		end),
		radius = found(function()
			return #workspace:GetPartBoundsInRadius(SpatialAt + Vector3.new(30, 0, 20), 2, OverlapParams.new())
		end),
		inPartLine = found(function()
			return #workspace:GetPartsInPart(Spatial.First, OverlapParams.new())
		end),
		inPartStack = found(function()
			return #workspace:GetPartsInPart(Spatial.Stack[1], OverlapParams.new())
		end),
		rayStack = found(function()
			return ray(SpatialAt + Vector3.new(0, 10, 0))
		end),
		rayOne = found(function()
			return ray(SpatialAt + Vector3.new(30, 10, 20))
		end),
		rayInclude = found(function()
			return ray(SpatialAt + Vector3.new(0, 10, 0), Include)
		end),
		rayExclude = found(function()
			return ray(SpatialAt + Vector3.new(30, 10, 20), Exclude)
		end),
		sphereStack = found(function()
			local Hit = workspace:Spherecast(SpatialAt + Vector3.new(0, 10, 0), 1, Vector3.new(0, -20, 0))
			return if Hit then Hit.Instance.Name else "miss"
		end),
		blockStack = found(function()
			local Hit = workspace:Blockcast(CFrame.new(SpatialAt + Vector3.new(0, 10, 0)), Vector3.new(2, 2, 2), Vector3.new(0, -20, 0))
			return if Hit then Hit.Instance.Name else "miss"
		end),
	}
	for _, Key in { "Alone", "Big", "Ball", "Beside", "Six", "Twelve", "Forty", "Rod", "Slab" } do
		Meta.queries["inPart" .. Key] = found(function()
			return #workspace:GetPartsInPart(loner(S, Key), OverlapParams.new())
		end)
	end
	local IncludeOverlap = OverlapParams.new()
	IncludeOverlap.FilterType = Enum.RaycastFilterType.Include
	IncludeOverlap.FilterDescendantsInstances = { Spatial.Stack[1] }
	Meta.queries.inPartIncludeMiss = found(function()
		return #workspace:GetPartsInPart(loner(S, "Alone"), IncludeOverlap)
	end)
	if Options.Language and Options.Language.interp then
		Meta.compiledName = Options.Language.interp.CompiledName()
		Meta.optimization = if Meta.compiledName == "CompiledName" then 2 else 1
	end
	if Options.Language and Options.Language.native then
		Meta.nativeCompiledName = Options.Language.native.CompiledName()
	end
	return { meta = Meta, rows = Rows }
end

-- Takes this corpus's scene out of the DataModel: a client running the groups as jobs of one session would otherwise keep every
-- job's scene, and a later group's queries would meet every earlier group's parts (three stacks of 24 instead of one).
function Corpus.Cleanup()
	local S = Scene
	if not S then
		return
	end
	if S.Spatial then
		S.Spatial.Line.Parent:Destroy()
	end
	S.Root:Destroy()
	S.Gui:Destroy()
	if S.Loose then
		S.Loose:Destroy()
	end
	Scene = nil
end

Corpus.List = List
return Corpus
