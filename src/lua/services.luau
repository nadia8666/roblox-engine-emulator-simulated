--!nocheck
-- Rust engine: the service stubs of runtime/engine/services.luau that return script data (Players
-- lookups, 3D model helpers, humanoid stand-ins, Path2D, EditableImage, HttpService
-- JSON, ContentProvider, localization / marketplace / policy / social stubs, remotes). Kept in
-- Luau: they build script tables, may yield, and raise their errors at script level exactly as
-- the Luau runtime's do. Hot behaviours (DataModel, RunService, TweenService, ...) are Rust.
--
-- Runs with the shared script globals; R holds engine primitives (see lua/stubs.rs).

local R = ...
local Method = R.Method
local Getter = R.Getter
local ContentSetter = R.ContentSetter
local Create = R.Create
local RawSet = R.RawSet
local RawGet = R.RawGet
local Children = R.Children
local Missing = R.Missing
local Options = R.Options
local Cost = R.Cost
local Mute = R.Mute
local Charge = R.Charge
local Pack, Unpack = table.pack, table.unpack

-- Methods Studio does in one native call that this file does in Luau price themselves (cost.rs: the whole call, measured
-- in Studio); the Instance API's charges for the reads and writes made here meanwhile are dropped.
local function Priced(Body)
	return function(...)
		Mute(true)
		local Results = Pack(pcall(Body, ...))
		Mute(false)
		if not Results[1] then
			error(Results[2], 0)
		end
		return Unpack(Results, 2, Results.n)
	end
end

local Extras = setmetatable({}, { __mode = "k" })
local function Extra(Instance_)
	local Found = Extras[Instance_]
	if not Found then
		Found = {}
		Extras[Instance_] = Found
	end
	return Found
end

-- The error level of the script line that called a stub method: `x:Method()` reaches the stub
-- through the Instance __namecall (a C function, lua/raw.rs), one level further up than
-- `x.Method(x)`. `Level` is relative to the stub.
local function ScriptLevel(Level)
	local Source, Name = debug.info(Level + 1, "sn")
	if Source == "[C]" and Name == "__namecall" then
		return Level + 1
	end
	return Level
end

local function EmptyPages()
	return {
		IsFinished = true,
		GetCurrentPage = function()
			return {}
		end,
		AdvanceToNextPageAsync = function() end,
	}
end

local function EnumItem(EnumName, ItemName)
	return R.GetEnumItem(EnumName, ItemName)
end

local function LocalPlayer()
	return R.LocalPlayer()
end

-- Players

Method("Players", "GetUserThumbnailAsync", function(_, UserId, ThumbnailType, ThumbnailSize)
	local TypeName = if ThumbnailType then ThumbnailType.Name else "HeadShot"
	local SizeName = if ThumbnailSize then ThumbnailSize.Name else "Size420x420"
	local Width, Height = string.match(SizeName, "(%d+)x(%d+)")
	local Kind = if TypeName == "AvatarBust" then "AvatarBust" elseif TypeName == "AvatarThumbnail" then "Avatar" else "AvatarHeadShot"
	return `rbxthumb://type={Kind}&id={UserId}&w={Width or 420}&h={Height or 420}`, true
end)

-- Measured in Studio (edit mode) for the user ids the project's stories use: the accounts' names,
-- and for ids without an account the error below; other ids fall back to User<id> offline.
local KnownUserNames = {
	[1] = "Roblox", [2] = "John Doe", [3] = "Jane Doe", [6] = "qa", [8] = "Toolbox", [33] = "morgan",
	[50] = "elvisarea51", [77] = "EZWrighter", [88] = "raabs", [99] = "OneRaven", [101] = "mraab",
	[102] = "sean", [103] = "opi511", [104] = "Thomas H", [105] = "Zap", [156] = "builderman",
	[200] = "pjjexpert", [261] = "Shedletsky",
}
local UnknownUserIds = { [4] = true, [5] = true, [7] = true, [9] = true, [10] = true, [11] = true, [12] = true }
Method("Players", "GetNameFromUserIdAsync", function(_, UserId)
	local Player = LocalPlayer()
	if Player and RawGet(Player, "UserId") == UserId then
		return RawGet(Player, "Name")
	end
	if KnownUserNames[UserId] then
		return KnownUserNames[UserId]
	end
	if UnknownUserIds[UserId] then
		error("Players:GetUserIdFromNameAsync() failed: Unknown user", 0)
	end
	return `User{UserId}`
end)
local function HumanoidDescription()
	return Create("HumanoidDescription")
end
Method("Players", "GetHumanoidDescriptionFromUserId", HumanoidDescription)
Method("Players", "GetHumanoidDescriptionFromUserIdAsync", HumanoidDescription)
Method("Players", "GetHumanoidDescriptionFromOutfitId", HumanoidDescription)
Method("Players", "GetHumanoidDescriptionFromOutfitIdAsync", HumanoidDescription)
Method("Players", "GetFriendsAsync", function()
	return EmptyPages()
end)
-- A bare R15-less rig: a Model holding a Humanoid (no meshes offline).
local function HumanoidModel()
	local Model = Create("Model")
	local Humanoid = Create("Humanoid")
	R.SetParent(Humanoid, Model)
	return Model
end
for _, Name in { "CreateHumanoidModelFromDescription", "CreateHumanoidModelFromDescriptionAsync", "CreateHumanoidModelFromUserId", "CreateHumanoidModelFromUserIdAsync" } do
	Method("Players", Name, HumanoidModel)
end

-- UserService / TextChatService (offline: other users exist and may chat)
local UserInfoIds = { [1] = true, [2] = true, [3] = true, [8] = true, [33] = true, [50] = true, [77] = true, [156] = true, [200] = true, [261] = true }
local VerifiedUserIds = { [1] = true, [156] = true, [261] = true }
local function UserInfo(UserId)
	local Player = LocalPlayer()
	if Player and RawGet(Player, "UserId") == UserId then
		return { Id = UserId, Username = RawGet(Player, "Name"), DisplayName = RawGet(Player, "DisplayName"), HasVerifiedBadge = false }
	end
	if UserInfoIds[UserId] then
		return { Id = UserId, Username = KnownUserNames[UserId], DisplayName = KnownUserNames[UserId], HasVerifiedBadge = VerifiedUserIds[UserId] == true }
	end
	if KnownUserNames[UserId] or UnknownUserIds[UserId] then
		return nil
	end
	return { Id = UserId, Username = `User{UserId}`, DisplayName = `User{UserId}`, HasVerifiedBadge = false }
end
Method("UserService", "GetUserInfosByUserIdsAsync", function(_, UserIds)
	local Result = {}
	for _, UserId in UserIds or {} do
		local Info = UserInfo(UserId)
		if Info then
			table.insert(Result, Info)
		end
	end
	return Result
end)
Method("TextChatService", "CanUserChatAsync", function()
	return true
end)
Method("TextChatService", "CanUsersChatAsync", function()
	return true
end)
Method("TextChatService", "CanUsersDirectChatAsync", function(_, _RequesterUserId, UserIds)
	return table.clone(UserIds or {})
end)

-- 3D geometry used by viewport framing and model presentation.

local function PartBounds(Root, Basis)
	Basis = Basis or CFrame.new()
	local Min, Max = nil, nil
	local function Visit(Node)
		if Node:IsA("BasePart") then
			local Transform = Basis:Inverse() * RawGet(Node, "CFrame")
			local Half = RawGet(Node, "Size") / 2
			for X = -1, 1, 2 do
				for Y = -1, 1, 2 do
					for Z = -1, 1, 2 do
						local Point = Transform * Vector3.new(Half.X * X, Half.Y * Y, Half.Z * Z)
						Min = if Min then Min:Min(Point) else Point
						Max = if Max then Max:Max(Point) else Point
					end
				end
			end
		end
		for _, Child in Children(Node) do
			Visit(Child)
		end
	end
	Visit(Root)
	if not Min then
		return nil, Vector3.zero
	end
	return Basis * CFrame.new((Min + Max) / 2), Max - Min
end

Method("PVInstance", "GetPivot", Priced(function(Self)
	Charge(Cost.ModelQuery)
	if Self:IsA("BasePart") then
		return RawGet(Self, "CFrame") * RawGet(Self, "PivotOffset")
	end
	-- Measured (Studio 2026-10-16): a model's PrimaryPart, else its stored WorldPivotData (files,
	-- clones, PivotTo and WorldPivot writes set it), else the centre of its parts while it is unset.
	local Primary = if Self:IsA("Model") then RawGet(Self, "PrimaryPart") else nil
	if Primary then
		return Primary:GetPivot()
	end
	local Stored = if Self:IsA("Model") then RawGet(Self, "WorldPivotData", true) else Extra(Self).Pivot
	if Stored then
		return Stored
	end
	local Center = PartBounds(Self)
	return Center or CFrame.new()
end))
-- A non-part PVInstance's stored pivot: a model's WorldPivotData, any other's the emulator's own.
local function StorePivot(Self, Pivot)
	if Self:IsA("Model") then
		RawSet(Self, "WorldPivotData", Pivot, true)
	else
		Extra(Self).Pivot = Pivot
	end
end
local function StoredPivot(Self)
	return if Self:IsA("Model") then RawGet(Self, "WorldPivotData", true) else Extra(Self).Pivot
end
-- Moves many parts as one step: the assemblies they belong to settle once it is done (world/joints.rs),
-- instead of each write carrying its whole assembly along.
local function Together(Body)
	R.BeginMoves()
	local Ok, Error = pcall(Body)
	R.EndMoves()
	if not Ok then
		error(Error, 0)
	end
end
-- Every part under Self moves by Delta, and every nested model's stored pivot with it (measured: a nested model's stored
-- pivot moves with its ancestor).
local function MoveDescendants(Self, Delta)
	local Parts = 0
	for _, Descendant in Self:GetDescendants() do
		if Descendant:IsA("BasePart") then
			Parts += 1
			Descendant.CFrame = Delta * Descendant.CFrame
		elseif Descendant:IsA("Model") then
			local Inner = StoredPivot(Descendant)
			if Inner then
				StorePivot(Descendant, Delta * Inner)
			end
		end
	end
	return Parts
end
-- Measured (roblox-sim, Studio 0.746, 2026-11-07): after PivotTo the pivot is the target exactly, every call. Composing
-- Delta = Target * Pivot:Inverse() onto the old one instead let its float error compound: the inverse is the transpose, so a
-- pivot scaled by s came back scaled by s squared, and a model pivoted every frame (an attack effect's spin) lost its
-- rotation in about 25 frames (its start part went to [0 0 0; 0 1 0; 0 0 0], the parts it emitted to NaN).
Method("PVInstance", "PivotTo", Priced(function(Self, Target)
	if Self:IsA("BasePart") then
		-- Measured (Studio 2026-10-16): a part's PivotTo carries every part under it, through Folders, Attachments and
		-- Models, as one rigid move (an effect that moves its FX parts so: ree left a nested Slash 1,915 studs behind)
		local Delta = Target * Self:GetPivot():Inverse()
		local Below = 0
		Together(function()
			Self.CFrame = Target * RawGet(Self, "PivotOffset"):Inverse()
			Below = MoveDescendants(Self, Delta)
		end)
		Charge(Cost.PivotSingle + Below * if Self:IsDescendantOf(workspace) then Cost.PivotPart else Cost.PivotLoose)
	else
		local Delta = Target * Self:GetPivot():Inverse()
		local Stored = StoredPivot(Self)
		local Moved = 0
		Together(function()
			Moved = MoveDescendants(Self, Delta)
		end)
		Charge(Cost.PivotBase + Moved * if Self:IsDescendantOf(workspace) then Cost.PivotPart else Cost.PivotLoose)
		-- (a model whose pivot is its own stores the target; one whose PrimaryPart sets it carries its stored pivot along)
		if not (Self:IsA("Model") and RawGet(Self, "PrimaryPart")) then
			StorePivot(Self, Target)
		elseif Stored then
			StorePivot(Self, Delta * Stored)
		end
	end
end))
Method("Model", "GetBoundingBox", Priced(function(Self)
	Charge(Cost.ModelQuery)
	local Center, Size = PartBounds(Self, Self:GetPivot().Rotation)
	return Center or Self:GetPivot(), Size
end))
Method("Model", "GetExtentsSize", Priced(function(Self)
	Charge(Cost.ModelQuery)
	local _, Size = Self:GetBoundingBox()
	return Size
end))
-- The scale is a property, as in Roblox: files store it (serialized as ScaleFactor, read by rbx-dom as
-- Scale) and clones keep it, so a model saved at 0.01 reports 0.01 and ScaleTo(1) grows it 100x.
Method("Model", "GetScale", function(Self)
	Charge(Cost.ModelQuery)
	local Scale = RawGet(Self, "Scale")
	return if type(Scale) == "number" and Scale > 0 then Scale else 1
end)
-- ScaleTo scales every property with a geometric meaning (Model:ScaleTo's documentation: joint lengths,
-- particle sizes, sound roll-off; the Model Scale API announcement: "sizes, forces, speeds", Textures and
-- SurfaceGuis stretching like a Decal). Measured in Studio 2026-10-17 (ScaleTo 0.1, 1.6 and 0.5): every
-- property scaled below scales, and a Light's Range and a ParticleEmitter's or Beam's ZOffset keep theirs.
-- A Beam's or Trail's TextureLength is a
-- repeat count in Stretch mode and keeps it (devforum "ScaleTo incorrectly scales Trail.TextureLength",
-- April 2025: Roblox then scaled it in every mode, and the engineer who wrote ScaleTo answered that Stretch
-- should not scale and Wrap / Static should; Trail.WidthScale is a scale and keeps it too).
local function ScaledSequence(Sequence, Ratio)
	local Points = {}
	for _, Point in Sequence.Keypoints do
		table.insert(Points, NumberSequenceKeypoint.new(Point.Time, Point.Value * Ratio, Point.Envelope * Ratio))
	end
	return NumberSequence.new(Points)
end
local function ScaledUDim2(Size, Ratio)
	return UDim2.new(Size.X.Scale * Ratio, Size.X.Offset, Size.Y.Scale * Ratio, Size.Y.Offset)
end
local function ScaleEffect(Descendant, Ratio)
	if Descendant:IsA("ParticleEmitter") then
		Descendant.Size = ScaledSequence(Descendant.Size, Ratio)
		Descendant.Speed = NumberRange.new(Descendant.Speed.Min * Ratio, Descendant.Speed.Max * Ratio)
		Descendant.Acceleration *= Ratio
	elseif Descendant:IsA("Beam") then
		Descendant.Width0 *= Ratio
		Descendant.Width1 *= Ratio
		Descendant.CurveSize0 *= Ratio
		Descendant.CurveSize1 *= Ratio
		if Descendant.TextureMode ~= Enum.TextureMode.Stretch then
			Descendant.TextureLength *= Ratio
		end
	elseif Descendant:IsA("Trail") then
		Descendant.MinLength *= Ratio
		Descendant.MaxLength *= Ratio
		if Descendant.TextureMode ~= Enum.TextureMode.Stretch then
			Descendant.TextureLength *= Ratio
		end
	elseif Descendant:IsA("Fire") then
		Descendant.Size *= Ratio
		Descendant.Heat *= Ratio
	elseif Descendant:IsA("Smoke") then
		Descendant.Size *= Ratio
		Descendant.RiseVelocity *= Ratio
	elseif Descendant:IsA("Sound") then
		Descendant.RollOffMinDistance *= Ratio
		Descendant.RollOffMaxDistance *= Ratio
	elseif Descendant:IsA("Texture") then
		Descendant.StudsPerTileU *= Ratio
		Descendant.StudsPerTileV *= Ratio
		Descendant.OffsetStudsU *= Ratio
		Descendant.OffsetStudsV *= Ratio
	elseif Descendant:IsA("SurfaceGui") then
		if Descendant.SizingMode == Enum.SurfaceGuiSizingMode.PixelsPerStud then
			Descendant.PixelsPerStud /= Ratio
		end
	elseif Descendant:IsA("BillboardGui") then
		-- (a size's Scale is studs; its Offset pixels)
		Descendant.Size = ScaledUDim2(Descendant.Size, Ratio)
		Descendant.StudsOffset *= Ratio
		Descendant.StudsOffsetWorldSpace *= Ratio
	end
end
Method("Model", "ScaleTo", Priced(function(Self, Scale)
	assert(type(Scale) == "number" and Scale > 0 and Scale < math.huge, "Scale must be positive and finite")
	local Ratio = Scale / Self:GetScale()
	local Pivot = Self:GetPivot()
	local Looked, Parts, Attachments, Joints = 0, 0, 0, 0
	local function Scaled(Transform)
		return Transform.Rotation + Transform.Position * Ratio
	end
	Together(function()
		for _, Descendant in Self:GetDescendants() do
			Looked += 1
			ScaleEffect(Descendant, Ratio)
			if Descendant:IsA("BasePart") then
				Parts += 1
				Descendant.Size *= Ratio
				Descendant.CFrame = Pivot * Scaled(Pivot:ToObjectSpace(Descendant.CFrame))
				Descendant.PivotOffset = Scaled(Descendant.PivotOffset)
			elseif Descendant:IsA("Model") then
				-- (measured 2026-10-16: a nested model's stored pivot scales about the pivot like its parts)
				local Inner = StoredPivot(Descendant)
				if Inner then
					StorePivot(Descendant, Pivot * Scaled(Pivot:ToObjectSpace(Inner)))
				end
			elseif Descendant:IsA("Attachment") then
				Attachments += 1
				Descendant.CFrame = Scaled(Descendant.CFrame)
			elseif Descendant:IsA("JointInstance") then
				Joints += 1
				Descendant.C0 = Scaled(Descendant.C0)
				Descendant.C1 = Scaled(Descendant.C1)
			elseif Descendant:IsA("WeldConstraint") then
				-- its parts' relative CFrame, once it took one (State bit 2; world/joints.rs)
				if bit32.band(RawGet(Descendant, "State") or 0, 2) ~= 0 then
					RawSet(Descendant, "CFrame0", Scaled(RawGet(Descendant, "CFrame0")), true)
				end
			elseif Descendant:IsA("DataModelMesh") then
				-- Only a file mesh's Scale is in studs: the other shapes scale with their part's Size, so
				-- ScaleTo keeps it (measured in Studio 2026-10-11: Head and Sphere stay 1.25, FileMesh 2.5)
				if Descendant:IsA("SpecialMesh") and Descendant.MeshType == Enum.MeshType.FileMesh then
					Descendant.Scale *= Ratio
				end
				Descendant.Offset *= Ratio
			end
		end
	end)
	RawSet(Self, "Scale", Scale, true)
	-- (Studio looks at every descendant either way; a rescale costs more for the parts, attachments and joints it rewrites, a placed model's parts most)
	local Cost_ = Looked * Cost.ScaleScanned
	if Ratio ~= 1 then
		Cost_ += Parts * (if Self:IsDescendantOf(workspace) then Cost.ScalePartPlaced else Cost.ScalePartLoose) + Attachments * Cost.ScaleAttachment + Joints * Cost.ScaleJoint
	end
	Charge(Cost_)
end))
Method("Model", "GetPrimaryPartCFrame", function(Self)
	return Self:GetPivot()
end)
Method("Model", "SetPrimaryPartCFrame", function(Self, Target)
	Self:PivotTo(Target)
end)
Method("Model", "MoveTo", function(Self, Position)
	Self:PivotTo(CFrame.new(Position))
end)
Method("Model", "TranslateBy", function(Self, Offset)
	Self:PivotTo(Self:GetPivot() + Offset)
end)

-- Animator / AnimationTrack are Rust (src/animation)
Method("Humanoid", "SetStateEnabled", function(Self, State, Enabled)
	Extra(Self)[`_state:{tostring(State)}`] = Enabled
end)
Method("Humanoid", "GetStateEnabled", function(Self, State)
	return Extra(Self)[`_state:{tostring(State)}`] ~= false
end)
Method("Humanoid", "GetState", function()
	return EnumItem("HumanoidStateType", "None")
end)
for _, Name in { "ChangeState", "MoveTo", "Move", "ApplyDescription", "ApplyDescriptionAsync", "ApplyDescriptionReset", "BuildRigFromAttachments" } do
	Method("Humanoid", Name, function() end)
end
Method("Humanoid", "GetAccessories", function(Self)
	local Result = {}
	for _, Child in Children(Self.Parent or Self) do
		if Child:IsA("Accessory") then
			table.insert(Result, Child)
		end
	end
	return Result
end)
Method("Humanoid", "GetAppliedDescription", function()
	return Create("HumanoidDescription")
end)

-- Path2D keeps its control points (curves are not rendered offline)
local function ControlPoints(Self)
	local Store = Extra(Self)
	Store.ControlPoints = Store.ControlPoints or {}
	return Store.ControlPoints
end
Method("Path2D", "SetControlPoints", function(Self, Points)
	Extra(Self).ControlPoints = table.clone(Points or {})
end)
Method("Path2D", "GetControlPoints", function(Self)
	return table.clone(ControlPoints(Self))
end)
Method("Path2D", "GetControlPoint", function(Self, Index)
	return ControlPoints(Self)[Index]
end)
Method("Path2D", "InsertControlPoint", function(Self, Index, Point)
	table.insert(ControlPoints(Self), math.clamp(Index, 1, #ControlPoints(Self) + 1), Point)
end)
Method("Path2D", "RemoveControlPoint", function(Self, Index)
	table.remove(ControlPoints(Self), Index)
end)
Method("Path2D", "UpdateControlPoint", function(Self, Index, Point)
	ControlPoints(Self)[Index] = Point
end)
Method("Path2D", "GetMaxControlPoints", function()
	return 1024
end)

-- EditableImage: RGBA pixels (images/rs: an image loaded from an asset holds the asset's, decoded; a new
-- one starts transparent black). One never written keeps its asset's id, so a SurfaceAppearance built
-- from it draws the asset as Roblox shows it; once written, every SurfaceAppearance built from it is
-- pointed at a PNG snapshot of its pixels, one per resumption that wrote it, which the recorder plays
-- back as the map changing. The Draw methods draw nothing offline.
local SnapshotsDue = {}
local SnapshotScheduled = false

local function ImageSize(Image)
	local Size = RawGet(Image, "Size")
	return math.floor(Size.X), math.floor(Size.Y)
end

local function ImagePixels(Image)
	local Data = Extra(Image)
	if not Data.Pixels then
		local Width, Height = ImageSize(Image)
		Data.Pixels = buffer.create(Width * Height * 4)
	end
	return Data.Pixels
end

-- A snapshot of the image as it is now, as a map's content (nil when it could not be written): one
-- file until the image is written again, whatever shows it.
local function Snapshot(Image)
	local Data = Extra(Image)
	if Data.Snapshot then
		return Data.Snapshot
	end
	local Width, Height = ImageSize(Image)
	local Path, Problem = R.SaveImage(Width, Height, ImagePixels(Image))
	if not Path then
		warn(`EditableImage: its pixels could not be kept for the render ({Problem})`)
		return nil
	end
	Data.Snapshot = Content.fromUri(`file://{Path}`)
	return Data.Snapshot
end

-- (set while a snapshot is assigned to an image's users: their Content setters keep it registered)
local Refreshing = false

local function TakeSnapshots()
	SnapshotScheduled = false
	local Due = SnapshotsDue
	SnapshotsDue = {}
	for Image in Due do
		local Users = Extra(Image).Users
		if not Users or next(Users) == nil then
			continue
		end
		local Map = Snapshot(Image)
		if Map then
			for Surface, Properties in Users do
				for Property in Properties do
					if Surface:IsA("SurfaceAppearance") then
						-- (its maps are read-only to scripts)
						RawSet(Surface, Property, Map, false)
					else
						-- through the property's setter, which a Content view of a string id writes through
						Refreshing = true
						local Ok, Problem = pcall(function()
							Surface[Property] = Map
						end)
						Refreshing = false
						if not Ok then
							warn(`EditableImage: {Surface:GetFullName()}.{Property} did not take its snapshot ({Problem})`)
						end
					end
				end
			end
		end
	end
end

local function Written(Image)
	local Data = Extra(Image)
	Data.Source = nil
	Data.Snapshot = nil
	if Data.Users and next(Data.Users) ~= nil then
		SnapshotsDue[Image] = true
		if not SnapshotScheduled then
			SnapshotScheduled = true
			task.defer(TakeSnapshots)
		end
	end
end

-- The rows of a Position/Size rectangle inside the image: (x, y, width, height), clipped.
local function Rectangle(Image, Position, Size)
	local Width, Height = ImageSize(Image)
	local X = math.clamp(math.floor(if Position then Position.X else 0), 0, Width)
	local Y = math.clamp(math.floor(if Position then Position.Y else 0), 0, Height)
	local W = math.clamp(math.floor(if Size then Size.X else Width), 0, Width - X)
	local H = math.clamp(math.floor(if Size then Size.Y else Height), 0, Height - Y)
	return X, Y, W, H, Width
end

for _, Name in { "DrawTriangle", "DrawImageProjected" } do
	Method("EditableImage", Name, function(Self)
		Extra(Self).Source = nil
		Extra(Self).Snapshot = nil
	end)
end

-- One pixel of an ImageCombineType, channels 0-1 (Roblox's ImageCombineType reference): the result's
-- r, g, b, a.
local function Combine(Kind, SR, SG, SB, SA, DR, DG, DB, DA)
	if Kind == "Overwrite" then
		return SR, SG, SB, SA
	elseif Kind == "AlphaBlend" then
		return SR * SA + DR * (1 - SA), SG * SA + DG * (1 - SA), SB * SA + DB * (1 - SA), SA + DA * (1 - SA)
	elseif Kind == "Add" then
		return DR + SR * SA, DG + SG * SA, DB + SB * SA, DA + SA
	elseif Kind == "Subtract" then
		return DR - SR * SA, DG - SG * SA, DB - SB * SA, DA - SA
	elseif Kind == "Multiply" then
		return DR * SR, DG * SG, DB * SB, DA * SA
	end
	-- BlendSourceOver (the default) and NormalMapBlend's blend
	local A = SA + DA * (1 - SA)
	if A <= 0 then
		return 0, 0, 0, 0
	end
	return (SR * SA + DR * DA * (1 - SA)) / A, (SG * SA + DG * DA * (1 - SA)) / A, (SB * SA + DB * DA * (1 - SA)) / A, A
end

local function Byte(Value)
	return math.clamp(math.floor(Value * 255 + 0.5), 0, 255)
end

-- Combines Source(x, y) (r, g, b, a in 0-1, or nil to leave the pixel) into the clipped rectangle.
local function Draw(Self, X0, Y0, W0, H0, CombineType, Source)
	local Width, Height = ImageSize(Self)
	local X1, Y1 = math.max(X0, 0), math.max(Y0, 0)
	local X2, Y2 = math.min(X0 + W0, Width), math.min(Y0 + H0, Height)
	if X2 <= X1 or Y2 <= Y1 then
		return
	end
	local Kind = if typeof(CombineType) == "EnumItem" then CombineType.Name else "BlendSourceOver"
	local Pixels = ImagePixels(Self)
	for Y = Y1, Y2 - 1 do
		for X = X1, X2 - 1 do
			local SR, SG, SB, SA = Source(X - X0, Y - Y0)
			if SR then
				local Offset = (Y * Width + X) * 4
				local R, G, B, A = Combine(
					Kind, SR, SG, SB, SA,
					buffer.readu8(Pixels, Offset) / 255, buffer.readu8(Pixels, Offset + 1) / 255,
					buffer.readu8(Pixels, Offset + 2) / 255, buffer.readu8(Pixels, Offset + 3) / 255
				)
				buffer.writeu8(Pixels, Offset, Byte(R))
				buffer.writeu8(Pixels, Offset + 1, Byte(G))
				buffer.writeu8(Pixels, Offset + 2, Byte(B))
				buffer.writeu8(Pixels, Offset + 3, Byte(A))
			end
		end
	end
	Written(Self)
end

Method("EditableImage", "DrawRectangle", function(Self, Position, Size, Color, Transparency, CombineType)
	local R, G, B, A = Color.R, Color.G, Color.B, 1 - math.clamp(Transparency or 0, 0, 1)
	Draw(Self, math.floor(Position.X), math.floor(Position.Y), math.floor(Size.X), math.floor(Size.Y), CombineType, function()
		return R, G, B, A
	end)
end)

Method("EditableImage", "DrawImage", function(Self, Position, Image, CombineType)
	local Width, Height = ImageSize(Image)
	local Source = ImagePixels(Image)
	Draw(Self, math.floor(Position.X), math.floor(Position.Y), Width, Height, CombineType, function(X, Y)
		local Offset = (Y * Width + X) * 4
		return buffer.readu8(Source, Offset) / 255, buffer.readu8(Source, Offset + 1) / 255,
			buffer.readu8(Source, Offset + 2) / 255, buffer.readu8(Source, Offset + 3) / 255
	end)
end)
-- Whether a Draw call anti-aliases its edges: its AntiAliasing argument, Enabled by default (Roblox's
-- EditableImage reference gives DrawCircle and DrawLine `antiAliasing: AntiAliasing = Enabled`).
local function Smooth(AntiAliasing)
	return not (typeof(AntiAliasing) == "EnumItem" and AntiAliasing.Name == "Disabled")
end

Method("EditableImage", "DrawCircle", function(Self, Center, Radius, Color, Transparency, CombineType, AntiAliasing)
	local R, G, B, A = Color.R, Color.G, Color.B, 1 - math.clamp(Transparency or 0, 0, 1)
	local Soft = Smooth(AntiAliasing)
	local X0, Y0 = math.floor(Center.X - Radius - 1), math.floor(Center.Y - Radius - 1)
	local Span = math.ceil(Radius * 2) + 3
	Draw(Self, X0, Y0, Span, Span, CombineType, function(X, Y)
		-- the pixel's centre against the circle's edge: a pixel-wide ramp when smoothed
		local Distance = math.sqrt((X0 + X + 0.5 - Center.X) ^ 2 + (Y0 + Y + 0.5 - Center.Y) ^ 2)
		local Cover = if Soft then math.clamp(Radius + 0.5 - Distance, 0, 1) elseif Distance <= Radius then 1 else 0
		if Cover <= 0 then
			return nil
		end
		return R, G, B, A * Cover
	end)
end)

Method("EditableImage", "DrawLine", function(Self, P1, P2, Color, Transparency, CombineType, AntiAliasing)
	local R, G, B, A = Color.R, Color.G, Color.B, 1 - math.clamp(Transparency or 0, 0, 1)
	local Soft = Smooth(AntiAliasing)
	local X0, Y0 = math.floor(math.min(P1.X, P2.X)) - 1, math.floor(math.min(P1.Y, P2.Y)) - 1
	local W = math.floor(math.max(P1.X, P2.X)) - X0 + 2
	local H = math.floor(math.max(P1.Y, P2.Y)) - Y0 + 2
	local DX, DY = P2.X - P1.X, P2.Y - P1.Y
	local Length2 = DX * DX + DY * DY
	Draw(Self, X0, Y0, W, H, CombineType, function(X, Y)
		-- a one-pixel line: each pixel's distance from the segment (its centre to the nearest point on it)
		local PX, PY = X0 + X + 0.5 - (P1.X + 0.5), Y0 + Y + 0.5 - (P1.Y + 0.5)
		local T = if Length2 > 0 then math.clamp((PX * DX + PY * DY) / Length2, 0, 1) else 0
		local Distance = math.sqrt((PX - T * DX) ^ 2 + (PY - T * DY) ^ 2)
		local Cover = if Soft then math.clamp(1 - Distance, 0, 1) elseif Distance <= 0.5 then 1 else 0
		if Cover <= 0 then
			return nil
		end
		return R, G, B, A * Cover
	end)
end)

-- DrawImageTransformed(position, scale, rotation, image, options): the image scaled, turned by `rotation`
-- degrees about its PivotPoint (default its centre) and placed with that pivot at `position`, sampled
-- bilinearly (SamplingMode Pixelated: the nearest pixel), combined by options.CombineType (default
-- AlphaBlend), as Roblox's EditableImage reference and its samples describe it.
Method("EditableImage", "DrawImageTransformed", function(Self, Position, Scale, Rotation, Image, Options)
	Options = Options or {}
	local Width, Height = ImageSize(Image)
	local Source = ImagePixels(Image)
	local Pivot = Options.PivotPoint or Vector2.new(Width / 2, Height / 2)
	local SX, SY = Scale.X, Scale.Y
	if SX == 0 or SY == 0 then
		return
	end
	local Angle = math.rad(Rotation or 0)
	local C, S = math.cos(Angle), math.sin(Angle)
	local Nearest = typeof(Options.SamplingMode) == "EnumItem" and Options.SamplingMode.Name == "Pixelated"
	-- the destination box of the transformed image's corners
	local MinX, MinY, MaxX, MaxY = math.huge, math.huge, -math.huge, -math.huge
	for _, Corner in { { 0, 0 }, { Width, 0 }, { 0, Height }, { Width, Height } } do
		local LX, LY = (Corner[1] - Pivot.X) * SX, (Corner[2] - Pivot.Y) * SY
		local X, Y = Position.X + LX * C - LY * S, Position.Y + LX * S + LY * C
		MinX, MinY, MaxX, MaxY = math.min(MinX, X), math.min(MinY, Y), math.max(MaxX, X), math.max(MaxY, Y)
	end
	local X0, Y0 = math.floor(MinX), math.floor(MinY)
	local function Texel(X, Y)
		X, Y = math.clamp(X, 0, Width - 1), math.clamp(Y, 0, Height - 1)
		local Offset = (Y * Width + X) * 4
		return buffer.readu8(Source, Offset) / 255, buffer.readu8(Source, Offset + 1) / 255,
			buffer.readu8(Source, Offset + 2) / 255, buffer.readu8(Source, Offset + 3) / 255
	end
	local Combine = Options.CombineType or Enum.ImageCombineType.AlphaBlend
	Draw(Self, X0, Y0, math.ceil(MaxX) - X0 + 1, math.ceil(MaxY) - Y0 + 1, Combine, function(X, Y)
		-- the destination pixel's centre back into the source image
		local DX, DY = X0 + X + 0.5 - Position.X, Y0 + Y + 0.5 - Position.Y
		local U = (DX * C + DY * S) / SX + Pivot.X
		local V = (-DX * S + DY * C) / SY + Pivot.Y
		if U < 0 or V < 0 or U >= Width or V >= Height then
			return nil
		end
		if Nearest then
			return Texel(math.floor(U), math.floor(V))
		end
		local FU, FV = U - 0.5, V - 0.5
		local IU, IV = math.floor(FU), math.floor(FV)
		local TU, TV = FU - IU, FV - IV
		local R1, G1, B1, A1 = Texel(IU, IV)
		local R2, G2, B2, A2 = Texel(IU + 1, IV)
		local R3, G3, B3, A3 = Texel(IU, IV + 1)
		local R4, G4, B4, A4 = Texel(IU + 1, IV + 1)
		local function Mix(P, Q, Rr, Ss)
			return (P * (1 - TU) + Q * TU) * (1 - TV) + (Rr * (1 - TU) + Ss * TU) * TV
		end
		return Mix(R1, R2, R3, R4), Mix(G1, G2, G3, G4), Mix(B1, B2, B3, B4), Mix(A1, A2, A3, A4)
	end)
end)

Method("EditableImage", "Destroy", function(Self)
	local Data = Extra(Self)
	Data.Pixels = nil
	Data.Users = nil
end)
Method("EditableImage", "ReadPixelsBuffer", function(Self, Position, Size)
	local X, Y, W, H, Stride = Rectangle(Self, Position, Size)
	local Out = buffer.create(W * H * 4)
	local Pixels = ImagePixels(Self)
	for Row = 0, H - 1 do
		buffer.copy(Out, Row * W * 4, Pixels, ((Y + Row) * Stride + X) * 4, W * 4)
	end
	return Out
end)
Method("EditableImage", "WritePixelsBuffer", function(Self, Position, Size, Source)
	local X, Y, W, H, Stride = Rectangle(Self, Position, Size)
	local Pixels = ImagePixels(Self)
	local Rows = math.min(H, buffer.len(Source) // math.max(W * 4, 1))
	for Row = 0, Rows - 1 do
		buffer.copy(Pixels, ((Y + Row) * Stride + X) * 4, Source, Row * W * 4, W * 4)
	end
	Written(Self)
end)
local function CreateEditableImage(_, Settings)
	local Image = Create("EditableImage")
	local Size = if type(Settings) == "table" and Settings.Size then Settings.Size else Vector2.new(512, 512)
	RawSet(Image, "Size", Size, true)
	return Image
end
Method("AssetService", "CreateEditableImage", CreateEditableImage)
Method("AssetService", "CreateEditableImageAsync", function(Self, Source, Settings)
	local Uri = if typeof(Source) == "Content" and Source.SourceType == Enum.ContentSourceType.Uri
		then Source.Uri
		elseif type(Source) == "number" then `rbxassetid://{Source}`
		elseif type(Source) == "string" then Source
		else nil
	local Width, Height, Pixels, Problem = nil, nil, nil, nil
	if Uri then
		Width, Height, Pixels, Problem = R.LoadImage(Uri)
		if not Pixels then
			warn(`EditableImage: {Problem}; it starts blank`)
		end
	end
	local Image = CreateEditableImage(Self, if Pixels then { Size = Vector2.new(Width, Height) } else Settings)
	local Data = Extra(Image)
	Data.Source = Uri
	Data.Pixels = Pixels
	return Image
end)

-- CreateSurfaceAppearanceAsync: each map an EditableImage's content (Roblox takes no other); an image
-- loaded from an asset and never written gives the map that asset, a written one its snapshot, and
-- the map follows its later writes.
local SURFACE_MAPS = {
	ColorMap = "ColorMapContent",
	NormalMap = "NormalMapContent",
	RoughnessMap = "RoughnessMapContent",
	MetalnessMap = "MetalnessMapContent",
	EmissiveMask = "EmissiveMaskContent",
}
Method("AssetService", "CreateSurfaceAppearanceAsync", function(_, Maps)
	if type(Maps) ~= "table" then
		error(`invalid argument #1 to 'CreateSurfaceAppearanceAsync' (table expected, got {typeof(Maps)})`, ScriptLevel(2))
	end
	local Surface = Create("SurfaceAppearance")
	for Key, Property in SURFACE_MAPS do
		local Map = Maps[Key]
		if Map == nil then
			continue
		end
		if typeof(Map) ~= "Content" or Map.SourceType ~= Enum.ContentSourceType.Object then
			error(`CreateSurfaceAppearanceAsync: {Key} must be an EditableImage's Content`, 0)
		end
		local Image = Map.Object
		local Data = Extra(Image)
		Data.Users = Data.Users or setmetatable({}, { __mode = "k" })
		Data.Users[Surface] = Data.Users[Surface] or {}
		Data.Users[Surface][Property] = true
		local Drawn = if Data.Source then Content.fromUri(Data.Source) else Snapshot(Image)
		RawSet(Surface, Property, Drawn or Map, true)
	end
	return Surface
end)

-- Any other Content property that takes an image shows an EditableImage too (Roblox's EditableImage
-- reference: "EditableImage can be used in any Content property which takes an image, such as
-- ImageLabel.ImageContent or MeshPart.TextureContent", by Content.fromObject): the property holds the
-- image's asset while it is never written, else a snapshot of its pixels, and follows its later writes
-- as a SurfaceAppearance's maps do (a script reading it back gets that snapshot's Uri, not the Object).
local IMAGE_CONTENT = {
	ImageLabel = { "ImageContent" },
	ImageButton = { "ImageContent", "HoverImageContent", "PressedImageContent" },
	ImageHandleAdornment = { "ImageContent" },
	Decal = { "ColorMapContent", "TextureContent" },
	MeshPart = { "TextureContent" },
	FileMesh = { "TextureContent" },
	ParticleEmitter = { "TextureContent" },
	Beam = { "TextureContent" },
	Trail = { "TextureContent" },
}
for ClassName, Properties in IMAGE_CONTENT do
	for _, Property in Properties do
		ContentSetter(ClassName, Property, function(Self, Value)
			if Refreshing then
				return Value
			end
			local Shown = Extra(Self).Shown
			local Previous = Shown and Shown[Property]
			if Previous then
				local Users = Extra(Previous).Users
				if Users and Users[Self] then
					Users[Self][Property] = nil
				end
				Shown[Property] = nil
			end
			if typeof(Value) ~= "Content" or Value.SourceType ~= Enum.ContentSourceType.Object then
				return Value
			end
			local Image = Value.Object
			if typeof(Image) ~= "Instance" or not Image:IsA("EditableImage") then
				return Value
			end
			local Data = Extra(Image)
			Data.Users = Data.Users or setmetatable({}, { __mode = "k" })
			Data.Users[Self] = Data.Users[Self] or {}
			Data.Users[Self][Property] = true
			Extra(Self).Shown = Shown or {}
			Extra(Self).Shown[Property] = Image
			return (if Data.Source then Content.fromUri(Data.Source) else Snapshot(Image)) or Value
		end)
	end
end

-- Lighting: the sun and moon by ClockTime and GeographicLatitude (ror's fit to Studio's
-- GetSunDirection, within 1e-4: the sun circles the Z axis on a circle tilted latitude - 23.5
-- degrees; the moon mirrors it in X and Y)
local function SunDirection(Self)
	local HourAngle = ((RawGet(Self, "ClockTime") or 14) - 12) / 24 * math.pi * 2
	local Tilt = math.rad((RawGet(Self, "GeographicLatitude") or 0) - 23.5)
	return Vector3.new(-math.sin(HourAngle) * math.cos(Tilt), math.cos(HourAngle) * math.cos(Tilt), math.sin(Tilt))
end
Method("Lighting", "GetSunDirection", SunDirection)
Method("Lighting", "GetMoonDirection", function(Self)
	local Sun = SunDirection(Self)
	return Vector3.new(-Sun.X, -Sun.Y, Sun.Z)
end)
Method("Lighting", "GetMinutesAfterMidnight", function(Self)
	return (RawGet(Self, "ClockTime") or 14) * 60
end)
Method("Lighting", "SetMinutesAfterMidnight", function(Self, Minutes)
	Self.ClockTime = (Minutes / 60) % 24
end)

-- Player
Method("Player", "GetMouse", function()
	return R.Mouse
end)
Method("Player", "IsInGroup", function()
	return false
end)
Method("Player", "GetRankInGroup", function()
	return 0
end)
Method("Player", "GetRoleInGroup", function()
	return "Guest"
end)
Method("Player", "IsFriendsWith", function()
	return false
end)
Method("Player", "HasAppearanceLoaded", function()
	return true
end)
-- Character control without a character (Roblox's ControlModule binds Move at load)
for _, Name in { "Kick", "Move", "LoadCharacter", "LoadCharacterAsync", "RequestStreamAroundAsync" } do
	Method("Player", Name, function() end)
end
Method("Player", "DistanceFromCharacter", function()
	return 0
end)
Method("Player", "GetJoinData", function()
	return {}
end)
Method("Player", "GetFriendsOnline", function()
	return {}
end)
Method("Player", "IsVerified", function()
	return false
end)
Method("Player", "GetNetworkPing", function()
	return 0.05
end)

-- (HttpService JSON and requests are Rust: services/mod.rs)

-- ContentProvider
-- (it waits for the animation assets it asks for: animation/mod.rs preload)
Method("ContentProvider", "PreloadAsync", function(_, Items, Callback)
	local Wait = R.PreloadAnimations(Items)
	if Wait > 0 then
		task.wait(Wait)
	else
		task.wait()
	end
	if Callback then
		for _, Item in Items do
			local Id = if type(Item) == "string" then Item else tostring(Item)
			Callback(Id, EnumItem("AssetFetchStatus", "Success"))
		end
	end
end)
Getter("ContentProvider", "RequestQueueSize", function()
	return 0
end)
Method("ContentProvider", "GetAssetFetchStatus", function()
	return EnumItem("AssetFetchStatus", "Success")
end)

-- KeyframeSequenceProvider:GetKeyframeSequenceAsync: after a yield, a new copy of the KeyframeSequence
-- the id names, found as AnimationTracks find theirs (animation/source.rs). An id that does not load
-- raises after the yield, without a position (the message is not measured). (RegisterKeyframeSequence
-- is Rust: animation/mod.rs)
Method("KeyframeSequenceProvider", "GetKeyframeSequenceAsync", function(_, AssetId)
	if AssetId == nil then
		error("Argument 1 missing or nil", 0)
	end
	task.wait()
	local Sequence, Message = R.KeyframeSequence(tostring(AssetId))
	if not Sequence then
		error(Message, 0)
	end
	return Sequence
end)

-- TextService: family metadata of a font family asset / json path
Method("TextService", "GetFamilyInfoAsync", function(_, AssetId)
	local Id = tostring(AssetId)
	return {
		Name = string.match(Id, "([^/]+)%.json$") or Id,
		Faces = {
			{ Name = "Regular", Weight = EnumItem("FontWeight", "Regular"), Style = EnumItem("FontStyle", "Normal") },
		},
	}
end)

-- SharedTableRegistry: a name holds one SharedTable, made on the first Get (measured in Studio 2026-10-23: a
-- fresh name returns a SharedTable, the same one on every call; SetSharedTable registers a given one). Without
-- Actors there is nothing to share it with, so a SharedTable is a plain table here.
local SharedTables = {}
Method("SharedTableRegistry", "GetSharedTable", function(_, Name)
	local Found = SharedTables[Name]
	if Found == nil then
		Found = SharedTable.new()
		SharedTables[Name] = Found
	end
	return Found
end)
Method("SharedTableRegistry", "SetSharedTable", function(_, Name, Value)
	SharedTables[Name] = Value
end)

-- (CollectionService:GetAllTags is Rust: services/mod.rs)

-- GuiService.TopbarInset
Getter("GuiService", "TopbarInset", function()
	if Options.TopbarInset > 0 then
		return Rect.new(0, 0, Options.ViewportWidth, Options.TopbarInset)
	end
	return Rect.new(0, 0, 0, 0)
end)

-- GuiService:GetInsetArea(insets): the screen less the insets, as a Rect. The emulated screen has no device cutout, so None and
-- DeviceSafeInsets are the whole viewport and the core-UI insets leave the topbar out of the top (Roblox's documentation; not yet
-- measured in Studio: a panel's backdrop sizes itself from it).
Method("GuiService", "GetInsetArea", function(_, Insets)
	local Name = if Insets then Insets.Name else "None"
	local Top = if (Name == "CoreUISafeInsets" or Name == "TopbarSafeInsets") and Options.TopbarInset > 0 then Options.TopbarInset else 0
	return Rect.new(0, Top, Options.ViewportWidth, Options.ViewportHeight)
end)

-- Localization / Marketplace / Policy / social stubs
local TranslatorMethods = {
	Translate = function(_, _Context, Text)
		return Text
	end,
	FormatByKey = function(_, Key)
		return Key
	end,
	RobloxLocaleId = "en-us",
	LocaleId = "en-us",
}
Getter("LocalizationService", "RobloxLocaleId", function()
	return "en-us"
end)
Getter("LocalizationService", "SystemLocaleId", function()
	return "en-us"
end)
for _, Name in { "GetTranslatorForPlayerAsync", "GetTranslatorForPlayer", "GetTranslatorForLocaleAsync" } do
	Method("LocalizationService", Name, function()
		return TranslatorMethods
	end)
end
Method("LocalizationService", "GetCountryRegionForPlayerAsync", function()
	return "US"
end)

local function GetProductInfo(_, AssetId, InfoType)
	Missing("MarketplaceService", "GetProductInfo (mocked)")
	return {
		Name = `Product {AssetId}`,
		Description = "",
		PriceInRobux = 0,
		IsForSale = true,
		IsPublicDomain = false,
		IsLimited = false,
		IsLimitedUnique = false,
		IconImageAssetId = 0,
		AssetId = AssetId,
		ProductId = AssetId,
		Creator = { Name = "Roblox", CreatorType = "Group", Id = 1 },
		ProductType = if InfoType then InfoType.Name else "Asset",
		Remaining = 0,
		Sales = 0,
	}
end
Method("MarketplaceService", "GetProductInfo", GetProductInfo)
Method("MarketplaceService", "GetProductInfoAsync", GetProductInfo)
for _, Name in { "UserOwnsGamePassAsync", "PlayerOwnsAsset", "PlayerOwnsBundle" } do
	Method("MarketplaceService", Name, function()
		return false
	end)
end
Method("MarketplaceService", "GetDeveloperProductsAsync", function()
	return EmptyPages()
end)
for _, Name in { "PromptPurchase", "PromptProductPurchase", "PromptGamePassPurchase", "PromptPremiumPurchase", "PromptSubscriptionPurchase", "PromptBundlePurchase" } do
	Method("MarketplaceService", Name, function() end)
end

Method("PolicyService", "GetPolicyInfoForPlayerAsync", function()
	return {
		AreAdsAllowed = true,
		ArePaidRandomItemsRestricted = false,
		AllowedExternalLinkReferences = { "Discord", "YouTube", "Twitch", "Facebook", "X" },
		IsContentSharingAllowed = true,
		IsEligibleToPurchaseSubscription = true,
		IsPaidItemTradingAllowed = true,
		IsSubjectToChinaPolicies = false,
	}
end)
Method("BadgeService", "UserHasBadgeAsync", function()
	return false
end)
Method("BadgeService", "GetBadgeInfoAsync", function()
	return { Name = "Badge", Description = "", IconImageId = 0, IsEnabled = true }
end)
Method("GroupService", "GetGroupInfoAsync", function(_, GroupId)
	return { Name = `Group {GroupId}`, Id = GroupId, Owner = { Name = "Owner", Id = 1 }, EmblemUrl = "", Roles = {} }
end)
Method("GroupService", "GetGroupsAsync", function()
	return {}
end)
-- Studio's answers for an offline player (an empty test place, 2026-10-23): no upcoming events, and a player who is in no group
Method("GroupService", "GetRolesInGroupAsync", function()
	return { IsMember = false, Roles = {} }
end)
Method("SocialService", "GetUpcomingExperienceEventsAsync", function()
	return {}
end)
Method("SocialService", "CanSendGameInviteAsync", function()
	return false
end)
Method("SocialService", "CanSendCallInviteAsync", function()
	return false
end)
Getter("VRService", "VREnabled", function()
	return false
end)
Method("TeleportService", "GetLocalPlayerTeleportData", function()
	return nil
end)
Method("TeleportService", "GetTeleportSetting", function()
	return nil
end)
Method("TeleportService", "SetTeleportSetting", function() end)
-- UserGameSettings (UserSettings().GameSettings): a client whose settings are as they start
for _, Name in { "GetCameraYInvert", "GetOnboardingCompleted", "InFullScreen", "InStudioMode" } do
	Method("UserGameSettings", Name, function()
		return false
	end)
end
for _, Name in { "SetCameraYInvertVisible", "SetGamepadCameraSensitivityVisible", "SetOnboardingCompleted" } do
	Method("UserGameSettings", Name, function() end)
end
for _, Name in { "LogCustomEvent", "LogEconomyEvent", "LogProgressionEvent", "LogFunnelStepEvent", "LogOnboardingFunnelStepEvent" } do
	Method("AnalyticsService", Name, function() end)
end
-- (SoundService:PlayLocalSound is Rust: engine::world reports the play)

-- Remotes never reach a server offline; invocations yield forever like a dropped connection.
Method("RemoteFunction", "InvokeServer", function(Self)
	Missing("RemoteFunction", `InvokeServer ({Self:GetFullName()}) never returns offline`)
	return coroutine.yield()
end)
Method("BindableFunction", "Invoke", function(Self, ...)
	local Callback = R.GetCallback(Self, "OnInvoke")
	if not Callback then
		return coroutine.yield()
	end
	if Self:GetActor() then
		Charge(Cost.ActorInvoke) -- (a call into an Actor's VM: 1.0 us in Studio)
	end
	-- (the arguments and results are copied as Roblox copies them: tables, buffers, threads; lua/bindable.rs)
	return R.Copied(Callback(R.Copied(...)))
end)

-- LogService:Output, Info, Warn, Error and Log: a line of their MessageType (Output: print's, Info: MessageInfo, Warn: warn's,
-- Error: an error line, then a thrown error table), its `{key}` placeholders filled from the context. Measured in a playing
-- client (Studio 0.744; a server's game script took the same lines and order) beside Roblox's LogService documentation.
-- A placeholder runs from `{` to the next `}` and takes the context's value at that text (the table's own lookup: a
-- metatable's __index answers), written as tostring writes it (an Instance its Name); a key the context lacks keeps its
-- placeholder; `{{` and `}}` write one brace, and a lone `}` stays. Without a context table (none, or a number, a string, an
-- Instance) the message is left as it is, braces included.
local function LogRender(Template, Context)
	local Out = {}
	local Index, Length = 1, #Template
	while Index <= Length do
		local Next = string.find(Template, "[{}]", Index)
		if not Next then
			table.insert(Out, string.sub(Template, Index))
			break
		end
		table.insert(Out, string.sub(Template, Index, Next - 1))
		local Brace = string.sub(Template, Next, Next)
		if string.sub(Template, Next + 1, Next + 1) == Brace then
			table.insert(Out, Brace)
			Index = Next + 2
		elseif Brace == "}" then
			table.insert(Out, "}")
			Index = Next + 1
		else
			local Close = string.find(Template, "}", Next + 1, true)
			if not Close then
				table.insert(Out, string.sub(Template, Next))
				break
			end
			local Value = Context[string.sub(Template, Next + 1, Close - 1)]
			table.insert(Out, if Value == nil then string.sub(Template, Next, Close) else tostring(Value))
			Index = Close + 1
		end
	end
	return table.concat(Out)
end

-- The context as MessageOut and a thrown error carry it (measured): a new table each time, the given table's own keys (not a
-- metatable's) with their values copied. A context whose keys are all strings keeps its values' types: a function becomes
-- "<function>", an Instance its full name, a table a copy of its own (its keys as they are), and one that holds itself its
-- tostring text; a thread, an Enum and a plain userdata are left out, other values (numbers, strings, datatypes, EnumItems,
-- buffers) kept. A context with any other key (an array part, a number key) keeps strings, numbers and booleans and writes
-- every other value as tostring does (an Instance its Name, a Vector3 "1, 2, 3", a table "table: 0x..."). A number key becomes
-- its whole part's text (2.7 "2"; past int32's range "-2147483648"), a string key of the same text wins over it, other keys are
-- left out.
local function LogContextValue(Value, Path)
	local Kind = typeof(Value)
	if Kind == "function" then
		return "<function>"
	elseif Kind == "Instance" then
		return Value:GetFullName()
	elseif Kind == "table" then
		if Path[Value] then
			return nil, true
		end
		Path[Value] = true
		local Copy = {}
		for Key, Item in next, Value do
			local Converted, Cycle = LogContextValue(Item, Path)
			if Cycle then
				return nil, true
			end
			if Converted ~= nil then
				Copy[Key] = Converted
			end
		end
		Path[Value] = nil
		return Copy
	elseif Kind == "thread" or Kind == "Enum" or Kind == "userdata" then
		return nil
	end
	return Value
end
local function LogContext(Given)
	local Mixed = false
	for Key in next, Given do
		if type(Key) ~= "string" then
			Mixed = true
			break
		end
	end
	local Out, Numbered = {}, {}
	for Key, Value in next, Given do
		local Kind = type(Key)
		if Kind == "string" or Kind == "number" then
			local Converted, Cycle
			if Mixed then
				local ValueKind = type(Value)
				Converted = if ValueKind == "string" or ValueKind == "number" or ValueKind == "boolean" then Value else tostring(Value)
			else
				Converted, Cycle = LogContextValue(Value, {})
				if Cycle then
					Converted = tostring(Value)
				end
			end
			if Converted ~= nil then
				if Kind == "string" then
					Out[Key] = Converted
				else
					local Text = if Key == Key and Key > -2147483649 and Key < 2147483648
						then string.format("%d", if Key < 0 then math.ceil(Key) else math.floor(Key))
						else "-2147483648"
					Numbered[Text] = Converted
				end
			end
		end
	end
	for Text, Value in Numbered do
		if Out[Text] == nil then
			Out[Text] = Value
		end
	end
	return Out
end

-- The message as text (measured): a string as it is and a number as tostring writes it; anything else warns with the script's
-- stack ("argument #1 expects a string, but boolean was passed") and is then written as the engine writes that type: a table
-- "{}", an Instance "Instance", a function "function", an Enum "nil", the rest as tostring does (a boolean, a Vector3).
local function LogMessage(Message, Argument)
	local Kind = type(Message)
	if Kind == "string" then
		return Message
	elseif Kind == "number" then
		return tostring(Message)
	end
	local Type = typeof(Message)
	R.LogWarn(`argument #{Argument} expects a string, but {Type} was passed`, debug.traceback())
	if Type == "table" then
		return "{}"
	elseif Type == "Instance" then
		return "Instance"
	elseif Type == "function" then
		return "function"
	elseif Type == "Enum" then
		return "nil"
	end
	return tostring(Message)
end

local function LogSelf(Self, Member)
	if typeof(Self) ~= "Instance" or not Self:IsA("LogService") then
		error(`Expected ':' not '.' calling member function {Member}`, 0)
	end
end

-- The thrown error's tostring (measured): its message when that is a string or a number, else "Error occurred"
local function LogErrorText(Self)
	local Message = rawget(Self, "message")
	local Text = if type(Message) == "string" then Message elseif type(Message) == "number" then tostring(Message) else "Error occurred"
	R.LogErrorText(Text)
	return Text
end

-- One line: the message checked (`Position`: its argument's place counting the service, 2 or 3), written and its placeholders
-- filled; the line logged with a copy of the context. Returns the text and the template.
local function LogLine(Kind, Message, Given, Position)
	if Message == nil then
		error(`Argument {Position} missing or nil`, 0)
	end
	local Template = LogMessage(Message, Position - 1)
	local Context = if type(Given) == "table" then Given else nil
	local Text = if Context then LogRender(Template, Context) else Template
	R.LogLine(Kind, Text, if Context then LogContext(Context) else nil)
	Charge(Cost.LogCall)
	return Text, Template, Context
end

-- An error line, then the error table, thrown (measured): `message` (the text), `template` (the message as given), `context`
-- (a copy of its own; an empty table without one), `stack` ("Stack Begin", a `Script '<chunk>', Line <n>` line per frame of the
-- calling script, "Stack End"), and a metatable of its own whose __tostring gives the message. Not caught, it logs no other
-- error line: Engine::uncaught_log_error.
local function LogError(Message, Given, Position)
	local Text, Template, Context = LogLine("error", Message, Given, Position)
	local Thrown = {}
	Thrown.message = Text
	Thrown.stack = R.LogStack(debug.traceback())
	Thrown.template = Template
	Thrown.context = if Context then LogContext(Context) else {}
	setmetatable(Thrown, { __tostring = LogErrorText })
	error(Thrown, 0)
end

for Member, Kind in { Output = "print", Info = "info", Warn = "warn" } do
	Method("LogService", Member, Priced(function(Self, Message, Context)
		LogSelf(Self, Member)
		LogLine(Kind, Message, Context, 2)
	end))
end
Method("LogService", "Error", Priced(function(Self, Message, Context)
	LogSelf(Self, "Error")
	LogError(Message, Context, 2)
end))
-- Log(messageType, message, context): an EnumItem of MessageType, its name or its value (measured: "MessageInfo" and 1 are
-- MessageInfo; another enum's item "Unable to cast Material to MessageType", 7 "Unable to cast double to MessageType")
local LogKinds = { MessageOutput = "print", MessageInfo = "info", MessageWarning = "warn", MessageError = "error" }
Method("LogService", "Log", Priced(function(Self, MessageType, Message, Context)
	LogSelf(Self, "Log")
	if MessageType == nil then
		error("Argument 2 missing or nil", 0)
	end
	local Item = nil
	for _, Candidate in Enum.MessageType:GetEnumItems() do
		if Candidate == MessageType or Candidate.Name == MessageType or Candidate.Value == MessageType then
			Item = Candidate
			break
		end
	end
	if not Item then
		local Type = typeof(MessageType)
		local Name = if Type == "number" then "double" elseif Type == "EnumItem" then tostring(MessageType.EnumType) else Type
		error(`Unable to cast {Name} to MessageType`, 0)
	end
	if Item.Name == "MessageError" then
		LogError(Message, Context, 3)
	end
	LogLine(LogKinds[Item.Name], Message, Context, 3)
end))

return true
