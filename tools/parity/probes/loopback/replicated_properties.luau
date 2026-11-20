-- parity: loopback
-- T7: which properties replicate: the server makes instances of a few classes in ReplicatedStorage with each listed property
-- written to a value that is not its default, once before the player joins (the join's copy) and once after (as changes),
-- and the client says, per instance, which properties arrived as written and what the others read. Attributes and tags too.
local Storage = game:GetService("ReplicatedStorage")
local CollectionService = game:GetService("CollectionService")
local function Classes(Server)
	return {
		{ "Part", {
			Anchored = true, CanCollide = false, CanTouch = false, CanQuery = false, Transparency = 0.5, Reflectance = 0.25,
			Color = Color3.fromRGB(10, 200, 30), Size = Vector3.new(2, 3, 4), CFrame = CFrame.new(5, 6, 7) * CFrame.Angles(0, 1, 0),
			Material = Enum.Material.Wood, Locked = true, CastShadow = false, Massless = true, CollisionGroup = "Default",
			Archivable = false, Shape = Enum.PartType.Ball, TopSurface = Enum.SurfaceType.Studs, RootPriority = 3,
			CustomPhysicalProperties = PhysicalProperties.new(2, 0.5, 0.5), EnableFluidForces = false, LocalTransparencyModifier = 0.5,
		} },
		{ "Folder", { Archivable = false } },
		{ "StringValue", { Value = "text" } },
		{ "NumberValue", { Value = 2.5 } },
		{ "BoolValue", { Value = true } },
		{ "CFrameValue", { Value = CFrame.new(1, 2, 3) } },
		{ "Color3Value", { Value = Color3.new(0.5, 0.25, 1) } },
		{ "ObjectValue", { Value = if Server then workspace else nil } },
		{ "Model", { WorldPivot = CFrame.new(1, 1, 1), LevelOfDetail = Enum.ModelLevelOfDetail.Disabled } },
		{ "Script", { Enabled = false, RunContext = Enum.RunContext.Client } },
		{ "LocalScript", { Enabled = false } },
		{ "Sound", { SoundId = "rbxassetid://1", Volume = 0.25, Looped = true, PlaybackSpeed = 1.5, RollOffMaxDistance = 50 } },
		{ "ParticleEmitter", { Rate = 3, Enabled = false, Lifetime = NumberRange.new(2, 3), Speed = NumberRange.new(4), LightEmission = 0.5 } },
		{ "PointLight", { Brightness = 3, Range = 12, Enabled = false, Shadows = true } },
		{ "Attachment", { CFrame = CFrame.new(0, 1, 0), Visible = true } },
		{ "ScreenGui", { Enabled = false, DisplayOrder = 4, IgnoreGuiInset = true, ResetOnSpawn = false } },
		{ "Frame", { Size = UDim2.fromOffset(40, 30), Position = UDim2.fromScale(0.5, 0.5), BackgroundColor3 = Color3.new(1, 0, 0), Visible = false, ZIndex = 3, Rotation = 15 } },
		{ "TextLabel", { Text = "hello", TextSize = 20, RichText = true, TextScaled = true, FontFace = Font.fromEnum(Enum.Font.Arcade), TextTransparency = 0.5 } },
		{ "Decal", { Texture = "rbxassetid://2", Transparency = 0.5, Face = Enum.NormalId.Top } },
		{ "Highlight", { FillColor = Color3.new(0, 1, 0), DepthMode = Enum.HighlightDepthMode.Occluded, Enabled = false } },
		{ "Configuration", {} },
		{ "Weld", { C0 = CFrame.new(0, 2, 0), Enabled = false } },
		{ "BodyVelocity", { Velocity = Vector3.new(1, 2, 3), MaxForce = Vector3.new(4, 5, 6) } },
		{ "ProximityPrompt", { ActionText = "Act", HoldDuration = 2, MaxActivationDistance = 20, Enabled = false } },
		{ "Animation", { AnimationId = "rbxassetid://3" } },
	}
end
local function Same(A, B)
	if typeof(A) == "number" and typeof(B) == "number" then
		return math.abs(A - B) < 1e-4
	end
	return A == B
end
local function Text(Value)
	local Kind = typeof(Value)
	if Kind == "number" then
		return P.num(Value)
	elseif Kind == "Instance" then
		return Value:GetFullName()
	end
	return Kind
end
local function Make(Tag)
	local Folder = Instance.new("Folder")
	Folder.Name = Tag
	for Index, Class in Classes(true) do
		local Item = Instance.new(Class[1])
		Item.Name = Index .. Class[1]
		for Name, Value in Class[2] do
			local Ok, Problem = pcall(function()
				Item[Name] = Value
			end)
			if not Ok then
				print(Tag .. " cannot write " .. Class[1] .. "." .. Name .. ": " .. P.err(Problem))
			end
		end
		Item:SetAttribute("Number", 5)
		Item:SetAttribute("Text", "a")
		CollectionService:AddTag(Item, "Tagged")
		Item.Parent = Folder
	end
	return Folder
end
--- server
local Before = Make("Before")
Before.Parent = Storage
P.player()
P.meet("joined")
local After = Instance.new("Folder")
After.Name = "After"
After.Parent = Storage
P.sync()
for _, Item in Make("Later"):GetChildren() do
	Item.Parent = After
end
P.meet("made")
--- client
P.meet("joined")
P.meet("made")
for _ = 1, 30 do
	task.wait()
end
for _, Tag in { "Before", "After" } do
	local Folder = Storage:FindFirstChild(Tag)
	for Index, Class in Classes(false) do
		local Item = Folder and Folder:FindFirstChild(Index .. Class[1])
		if not Item then
			print(Tag .. " " .. Class[1] .. " missing")
			continue
		end
		local Arrived, Other = {}, {}
		local Names = {}
		for Name in Class[2] do
			table.insert(Names, Name)
		end
		table.sort(Names)
		for _, Name in Names do
			local Ok, Value = pcall(function()
				return Item[Name]
			end)
			if not Ok then
				table.insert(Other, Name .. " unreadable")
			elseif Class[1] == "ObjectValue" and Name == "Value" then
				table.insert(Other, Name .. "=" .. Text(Value))
			elseif Same(Value, Class[2][Name]) then
				table.insert(Arrived, Name)
			elseif typeof(Value) == "CFrame" and Value:FuzzyEq(Class[2][Name], 1e-4) then
				-- (a CFrame's rotation can arrive rounded)
				table.insert(Arrived, Name .. "~")
			else
				table.insert(Other, Name .. "=" .. Text(Value))
			end
		end
		print(Tag .. " " .. Class[1] .. " arrived {" .. table.concat(Arrived, " ") .. "} not {" .. table.concat(Other, " ")
			.. "} attributes " .. tostring(Item:GetAttribute("Number")) .. " " .. tostring(Item:GetAttribute("Text"))
			.. " tagged " .. tostring(CollectionService:HasTag(Item, "Tagged")))
	end
end
