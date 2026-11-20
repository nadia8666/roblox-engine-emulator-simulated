-- What Studio 0.747's API adds, as a game script meets it: Enum.EasingStyle's Bezier and Spring and everything that takes
-- an EasingStyle, TweenInfo.new's argument checks, the Camera's projection properties, the new combine and alpha types of
-- EditableImage's Draw methods, a Capsule part, and number ranges of new properties. A class or member the capture's
-- engine does not have yet prints its error.
local TweenService = game:GetService("TweenService")
local Items = {}
for _, Item in Enum.EasingStyle:GetEnumItems() do
	table.insert(Items, Item.Name .. "=" .. Item.Value)
end
print("easingstyle " .. table.concat(Items, " "))
print("easingstyle-values " .. tostring(Enum.EasingStyle:FromValue(11)) .. " " .. tostring(Enum.EasingStyle:FromValue(12)) .. " " .. tostring(Enum.EasingStyle:FromValue(13)) .. " " .. tostring(Enum.EasingStyle:FromName("Spring")))

local Quad, In = Enum.EasingStyle.Quad, Enum.EasingDirection.In
local function Info(Label, ...)
	print("tweeninfo " .. Label .. " " .. P.try(TweenInfo.new, ...))
end
Info("()")
Info("(nil)", nil)
Info("(1)", 1)
Info("(1,nil)", 1, nil)
Info("(1,nil,In)", 1, nil, In)
Info("(1,Quad,nil)", 1, Quad, nil)
Info("(1,Quad,In,nil)", 1, Quad, In, nil)
Info("(1,Quad,In,2,nil)", 1, Quad, In, 2, nil)
Info("(1,Quad,In,2,true,nil)", 1, Quad, In, 2, true, nil)
Info("(1,Quad,In,2,true,1,extra)", 1, Quad, In, 2, true, 1, "extra")
Info("('1')", "1")
Info("(true)", true)
Info("(1,'Quad')", 1, "Quad")
Info("(1,3)", 1, 3)
Info("(1,2.5)", 1, 2.5)
Info("(1,13)", 1, 13)
Info("(1,-1)", 1, -1)
Info("(1,false)", 1, false)
Info("(1,In)", 1, In)
Info("(1,Quad,'In')", 1, Quad, "In")
Info("(1,Quad,0)", 1, Quad, 0)
Info("(1,Quad,1.9)", 1, Quad, 1.9)
Info("(1,Quad,3)", 1, Quad, 3)
Info("(1,Quad,Quad)", 1, Quad, Quad)
Info("(1,Quad,In,'3')", 1, Quad, In, "3")
Info("(1,Quad,In,2.7)", 1, Quad, In, 2.7)
Info("(1,Quad,In,2,1)", 1, Quad, In, 2, 1)
Info("(1,Quad,In,2,true,'0.5')", 1, Quad, In, 2, true, "0.5")
for _, Name in { "Spring", "Bezier" } do
	local Style = Enum.EasingStyle[Name]
	Info("(1," .. Name .. ")", 1, Style)
	Info("(1," .. Name .. " value)", 1, Style.Value)
	Info("(1,'" .. Name .. "')", 1, Name)
	Info("(1," .. Name .. ",'bad')", 1, Style, "bad")
	Info("('x'," .. Name .. ")", "x", Style)
	print("getvalue " .. Name .. " " .. P.try(TweenService.GetValue, TweenService, 0.5, Style, In) .. " | " .. P.try(TweenService.GetValue, TweenService, 0.5, Name, "In") .. " | " .. P.try(TweenService.GetValue, TweenService, 0.5, Style.Value, 0)
		.. " | " .. P.try(TweenService.GetValue, TweenService, 0.5, Style, "bad") .. " | " .. P.try(TweenService.GetValue, TweenService, nil, Style, In) .. " | " .. P.try(TweenService.GetValue, TweenService, 0.5, Style))
end
print("getvalue-casts " .. P.try(TweenService.GetValue, TweenService, 0.5, 13, 0) .. " | " .. P.try(TweenService.GetValue, TweenService, 0.5, In, In) .. " | " .. P.try(TweenService.GetValue, TweenService, 0.5, "Quad", "In")
	.. " | " .. P.try(TweenService.GetValue, TweenService, 0.5, 3, 0) .. " | " .. P.try(TweenService.GetValue, TweenService, 0.5, Quad, "bad") .. " | " .. P.try(TweenService.GetValue, TweenService, 0.5, nil, In))

-- UIPageLayout.EasingStyle: what a write of each leaves
local Layout = Instance.new("UIPageLayout")
local Changes = 0
Layout:GetPropertyChangedSignal("EasingStyle"):Connect(function()
	Changes += 1
end)
local function Assign(Object, Property, Value)
	local Ok, Problem = pcall(function()
		Object[Property] = Value
	end)
	local Read = Object[Property]
	return (if Ok then "ok" else "error " .. P.err(Problem)) .. " -> " .. (if type(Read) == "number" then P.f32(Read) else tostring(Read))
end
print("pagelayout " .. tostring(Layout.EasingStyle) .. " | " .. Assign(Layout, "EasingStyle", Quad) .. " | " .. Assign(Layout, "EasingStyle", Enum.EasingStyle.Spring) .. " | " .. Assign(Layout, "EasingStyle", "Bezier")
	.. " | " .. Assign(Layout, "EasingStyle", 12) .. " | " .. Assign(Layout, "EasingStyle", 13) .. " | " .. Assign(Layout, "EasingStyle", In) .. " | " .. Assign(Layout, "EasingStyle", "Sine"))
task.wait()
print("pagelayout-changes " .. Changes)

-- GuiObject's tween methods
local Holder = Instance.new("Folder")
Holder.Parent = workspace
local Frame = Instance.new("Frame")
Frame.Parent = Holder
local Loose = Instance.new("Frame")
local Out = Enum.EasingDirection.Out
for _, Name in { "Spring", "Bezier" } do
	local Style = Enum.EasingStyle[Name]
	print("guitween " .. Name .. " " .. P.try(Frame.TweenPosition, Frame, UDim2.fromOffset(5, 5), Out, Style, 0.05, true) .. " | " .. P.try(Frame.TweenSize, Frame, UDim2.fromOffset(5, 5), Out, Style, 0.05, true)
		.. " | " .. P.try(Frame.TweenSizeAndPosition, Frame, UDim2.fromOffset(5, 5), UDim2.fromOffset(5, 5), Out, Style, 0.05, true) .. " | " .. P.try(Frame.TweenPosition, Frame, UDim2.fromOffset(5, 5), "Out", Name, 0.05, true)
		.. " | " .. P.try(Frame.TweenPosition, Frame, UDim2.fromOffset(5, 5), Out, Style.Value, 0.05, true) .. " | " .. P.try(Loose.TweenPosition, Loose, UDim2.fromOffset(5, 5), Out, Style, 0.05, true))
end
print("guitween-casts " .. P.try(Frame.TweenPosition, Frame, "x", Out, Enum.EasingStyle.Spring, 0.05, true) .. " | " .. P.try(Frame.TweenPosition, Frame, nil, Out, Enum.EasingStyle.Spring, 0.05, true)
	.. " | " .. P.try(Frame.TweenPosition, Frame, UDim2.fromOffset(5, 5), "bad", Enum.EasingStyle.Spring, 0.05, true) .. " | " .. P.try(Frame.TweenPosition, Frame, UDim2.fromOffset(5, 5), Out, 13, 0.05, true)
	.. " | " .. P.try(Frame.TweenPosition, Frame, UDim2.fromOffset(5, 5), Out, Enum.EasingStyle.Spring, "x", true) .. " | " .. P.try(Frame.TweenSizeAndPosition, Frame, UDim2.fromOffset(5, 5), "x", Out, Quad, 0.05, true))
print("guitween-untouched " .. tostring(Frame.Position) .. " " .. tostring(Frame.Size))
print("guitween-loose " .. P.try(Loose.TweenPosition, Loose, UDim2.fromOffset(5, 5), Out, Quad, 0.05, true) .. " | " .. P.try(Loose.TweenSize, Loose, UDim2.fromOffset(5, 5)) .. " | " .. tostring(Loose.Position))
print("guitween-placed " .. P.try(Frame.TweenPosition, Frame, UDim2.fromOffset(5, 5), Out, Quad, 0.05, true))
Holder:Destroy()

-- Pose easing is another enum
local Pose = Instance.new("Pose")
print("pose " .. Assign(Pose, "EasingStyle", Enum.EasingStyle.Spring) .. " | " .. Assign(Pose, "EasingStyle", "Spring") .. " | " .. Assign(Pose, "EasingStyle", 12))

-- the Camera's projection
local Camera = Instance.new("Camera")
local Fired = 0
Camera.Changed:Connect(function()
	Fired += 1
end)
print("camera " .. P.try(function()
	return tostring(Camera.ProjectionType) .. " " .. tostring(Camera.OrthographicMode) .. " " .. P.f32(Camera.OrthographicSize) .. " " .. P.f32(Camera.ViewStretch)
end))
print("camera-writes " .. P.try(function()
	return Assign(Camera, "ProjectionType", Enum.ProjectionType.Orthographic) .. " | " .. Assign(Camera, "ProjectionType", "Orthographic") .. " | " .. Assign(Camera, "ProjectionType", 5) .. " | " .. Assign(Camera, "OrthographicMode", Enum.OrthographicMode.Horizontal)
		.. " | " .. Assign(Camera, "OrthographicMode", Quad) .. " | " .. Assign(Camera, "OrthographicSize", 10) .. " | " .. Assign(Camera, "OrthographicSize", "x") .. " | " .. Assign(Camera, "ViewStretch", 2)
end))
task.wait()
print("camera-changes " .. Fired)

-- a Capsule part
print("capsule " .. P.try(function()
	local Part = Instance.new("Part")
	Part.Shape = Enum.PartType.Capsule
	Part.Size = Vector3.new(6, 2, 4)
	Part.Anchored = true
	Part.CFrame = CFrame.new(1000, 500, 1000)
	Part.Parent = workspace
	local Params = RaycastParams.new()
	Params.FilterType = Enum.RaycastFilterType.Include
	Params.FilterDescendantsInstances = { Part }
	local function Ray(X, Z)
		local Hit = workspace:Raycast(Vector3.new(1000 + X, 510, 1000 + Z), Vector3.new(0, -20, 0), Params)
		return if Hit then P.f32(Hit.Distance) else "miss"
	end
	local Line = tostring(Part.Shape) .. " " .. tostring(Part.Size) .. " mass " .. P.f32(Part.Mass) .. " rays " .. Ray(0, 0) .. " " .. Ray(2.9, 1.9) .. " " .. Ray(3.1, 0) .. " " .. Ray(0, 2.1)
	Part:Destroy()
	return Line
end))

-- EditableImage's draw methods
print("editableimage " .. P.try(function()
	local AssetService = game:GetService("AssetService")
	local Image = AssetService:CreateEditableImage({ Size = Vector2.new(4, 4) })
	local Other = AssetService:CreateEditableImage({ Size = Vector2.new(2, 2) })
	local Overlay, Add = Enum.ImageCombineType.Overlay, Enum.ImageCombineType.Add
	local Lock, Default = Enum.ImageAlphaType.LockCanvasAlpha, Enum.ImageAlphaType.Default
	local Red = Color3.new(1, 0, 0)
	local Lines = {
		P.try(Image.DrawImage, Image, Vector2.zero, Other, Overlay),
		P.try(Image.DrawImage, Image, Vector2.zero, Other, "Darken"),
		P.try(Image.DrawImage, Image, Vector2.zero, Other, 14),
		P.try(Image.DrawImage, Image, Vector2.zero, Other, Add, Lock),
		P.try(Image.DrawImage, Image, Vector2.zero, Other, Overlay, Lock),
		P.try(Image.DrawImage, Image, Vector2.zero, Other, Add, Default),
		P.try(Image.DrawRectangle, Image, Vector2.zero, Vector2.one, Red, 0, Overlay),
		P.try(Image.DrawRectangle, Image, Vector2.zero, Vector2.one, Red, 0, Add, Enum.ImageAlphaType.LockCanvasColor),
		P.try(Image.DrawCircle, Image, Vector2.one, 1, Red, 0, Overlay),
		P.try(Image.DrawCircle, Image, Vector2.one, 1, Red, 0, Add, Enum.AntiAliasing.Enabled, Lock),
		P.try(Image.DrawLine, Image, Vector2.zero, Vector2.one, Red, 0, Overlay),
		P.try(Image.DrawLine, Image, Vector2.zero, Vector2.one, Red, 0, Add, Enum.AntiAliasing.Enabled, Lock),
		P.try(Image.DrawImageTransformed, Image, Vector2.one, Vector2.one, 0, Other, { CombineType = Overlay }),
		P.try(Image.DrawImageTransformed, Image, Vector2.one, Vector2.one, 0, Other, { CombineType = Add, AlphaBlendType = Lock }),
	}
	return table.concat(Lines, " | ")
end))

-- number ranges
print("ranges " .. P.try(function()
	local Emitter = Instance.new("ParticleEmitter")
	return Assign(Emitter, "TimeScale", -1) .. " | " .. Assign(Emitter, "TimeScale", 0.25) .. " | " .. Assign(Emitter, "TimeScale", 7)
end))
print("ranges-2d " .. P.try(function()
	local Emitter = Instance.new("ParticleEmitter2D")
	return Assign(Emitter, "TimeScale", 2) .. " | " .. Assign(Emitter, "TimeScale", -1) .. " | " .. Assign(Emitter, "Brightness", -5) .. " | " .. Assign(Emitter, "Brightness", 1e9) .. " | " .. Assign(Emitter, "Blend", 5)
		.. " | " .. Assign(Emitter, "Blend", -1) .. " | " .. Assign(Emitter, "FlipbookSizeX", 0) .. " | " .. Assign(Emitter, "FlipbookSizeY", 100000) .. " | " .. Assign(Emitter, "Rate", -5)
end))
print("ranges-blur " .. P.try(function()
	local Blur = Instance.new("UIBlur")
	return tostring(Blur.Strength) .. " " .. Assign(Blur, "Transparency", 1.5) .. " | " .. Assign(Blur, "Transparency", -0.5)
end))
print("ranges-deadzone " .. P.try(function()
	local Binding = Instance.new("InputBinding")
	return tostring(Binding.Deadzone) .. " | " .. Assign(Binding, "Deadzone", NumberRange.new(2, 3)) .. " | " .. Assign(Binding, "Deadzone", NumberRange.new(-3, 0.5))
end))

-- members the refresh adds to or takes from what a script reads
local function Read(Object, Property)
	return P.try(function()
		local Value = Object[Property]
		return if type(Value) == "number" then P.f32(Value) else tostring(Value)
	end)
end
print("reads " .. Read(game, "ServerRegion") .. " | " .. Read(workspace, "AuthorityMode") .. " | " .. Read(workspace.Terrain, "Decoration") .. " | " .. Read(workspace.Terrain, "GrassLength")
	.. " | " .. Read(Instance.new("Decal"), "ResampleMode") .. " | " .. Read(Instance.new("AudioDeviceInput"), "Active") .. " | " .. Read(Instance.new("Folder"), "ExplorerIconContent"))
print("gone " .. Read(workspace, "PrimalPhysicsSolver") .. " | " .. Read(workspace, "StreamingMinRadius") .. " | " .. Read(game:GetService("Players"), "BanningEnabled") .. " | " .. Read(Instance.new("Part"), "siz")
	.. " | " .. P.try(function()
		return Enum.PrimalPhysicsSolver
	end))

-- settings Studio 0.747's ReflectionService lists no more: there, and not a game script's to read or write
local Hidden = {}
for _, Entry in {
	{ workspace, "StreamingMinRadius", 128 }, { workspace, "StreamingTargetRadius", 512 }, { workspace, "StreamOutBehavior", Enum.StreamOutBehavior.LowMemory },
	{ workspace, "StreamingIntegrityMode", Enum.StreamingIntegrityMode.Disabled }, { workspace, "PredictiveStreamingMode", Enum.PredictiveStreamingMode.Default },
	{ workspace, "PlayerScriptsUseInputActionSystem", Enum.RolloutState.Enabled }, { game:GetService("Players"), "BanningEnabled", false },
	{ game:GetService("Players"), "UseStrafingAnimations", true }, { game:GetService("StarterPlayer"), "CreateDefaultPlayerModule", false },
} do
	local Object, Property, Value = Entry[1], Entry[2], Entry[3]
	table.insert(Hidden, Property .. ": " .. Read(Object, Property) .. " / " .. P.try(function()
		Object[Property] = Value
	end))
end
for Index = 1, #Hidden, 3 do
	print("hidden " .. table.concat(Hidden, " | ", Index, math.min(Index + 2, #Hidden)))
end
