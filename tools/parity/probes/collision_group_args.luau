-- PhysicsService's collision group methods with arguments of other types and names at the edges: what a name argument
-- reads from a number, boolean, table, Vector3, Instance or function (the name a group gets), what the collidable
-- argument reads from a number, which error wins when a call breaks two rules, the name length in bytes, and
-- BasePart.CollisionGroup on a part in the Workspace. The probe removes every group it makes.
local PhysicsService = game:GetService("PhysicsService")

local function Value(Item)
	if type(Item) == "number" then
		return P.num(Item)
	end
	return (string.gsub(tostring(Item), "0x%x+", "ADDR"))
end

local function Names()
	local Ok, List = pcall(PhysicsService.GetRegisteredCollisionGroups, PhysicsService)
	if not Ok or type(List) ~= "table" then
		return {}
	end
	local Out = {}
	for _, Entry in List do
		table.insert(Out, if type(Entry) == "table" then Entry.name else Entry)
	end
	return Out
end

local function Call(Name, ...)
	local Arguments = table.pack(...)
	return P.try(function()
		return PhysicsService[Name](PhysicsService, table.unpack(Arguments, 1, Arguments.n))
	end)
end

local function Show(Label, Name, ...)
	print(Label .. " " .. Call(Name, ...))
end

local function Clean()
	pcall(function()
		for _, Name in Names() do
			if Name ~= "Default" then
				PhysicsService:UnregisterCollisionGroup(Name)
			end
		end
	end)
end

-- 1. the name a group gets from a name argument of another type
local Inputs = {
	{ "integer", 5 },
	{ "fraction", 2.5 },
	{ "negative-zero", -0 },
	{ "large", 1e20 },
	{ "2^53", 2 ^ 53 },
	{ "nan", 0 / 0 },
	{ "inf", math.huge },
	{ "true", true },
	{ "false", false },
	{ "empty-table", {} },
	{ "array", { 1, 2 } },
	{ "dictionary", { a = 1 } },
	{ "Vector3", Vector3.new(1, 2, 3) },
	{ "Instance", workspace },
	{ "EnumItem", Enum.Material.Plastic },
	{ "function", function() end },
	{ "minus-inf", -math.huge },
	{ "tenth", 0.1 },
	{ "third", 1 / 3 },
	{ "buffer", buffer.create(2) },
	{ "thread", coroutine.create(function() end) },
	{ "Enum", Enum.Material },
	{ "userdata", newproxy() },
	{ "Color3", Color3.new(1, 0, 0) },
	{ "UDim2", UDim2.new(1, 2, 3, 4) },
	{ "BrickColor", BrickColor.new("Bright red") },
}
for _, Input in Inputs do
	local Before = #Names()
	local Outcome = Call("RegisterCollisionGroup", Input[2])
	local After = Names()
	local Made = if #After > Before then string.format("%q", After[#After]) else "none"
	print("name-from-" .. Input[1] .. " " .. Outcome .. " | " .. P.text(Made) .. " | registered " .. Call("IsCollisionGroupRegistered", Input[2]))
	Clean()
end

-- 2. the collidable argument
Call("RegisterCollisionGroup", "ArgA")
for _, Input in { { "0", 0 }, { "1", 1 }, { "half", 0.5 }, { "minus-one", -1 }, { "nan", 0 / 0 }, { "true-string", "true" }, { "empty-string", "" }, { "table", {} }, { "Instance", workspace }, { "Vector3", Vector3.zero }, { "EnumItem", Enum.Material.Plastic }, { "false-string", "false" }, { "one-string", "1" }, { "zero-string", "0" }, { "TRUE-string", "TRUE" }, { "True-string", "True" }, { "spaced-string", " true" }, { "yes-string", "yes" }, { "function", function() end }, { "buffer", buffer.create(1) }, { "false", false } } do
	Call("CollisionGroupSetCollidable", "Default", "ArgA", true)
	local Outcome = Call("CollisionGroupSetCollidable", "Default", "ArgA", Input[2])
	print("collidable-from-" .. Input[1] .. " " .. Outcome .. " | " .. Call("CollisionGroupsAreCollidable", "Default", "ArgA"))
end
Call("CollisionGroupSetCollidable", "Default", "ArgA", true)

-- 3. the id argument of GetCollisionGroupName
for _, Input in { { "nil", nil }, { "string-1", "1" }, { "string-x", "x" }, { "fraction", 1.5 }, { "almost-32", 31.9 }, { "minus-half", -0.5 }, { "nan", 0 / 0 }, { "table", {} }, { "true", true }, { "0.4", 0.4 }, { "0.5", 0.5 }, { "-0.4", -0.4 }, { "30.5", 30.5 }, { "31.4", 31.4 }, { "31.5", 31.5 }, { "inf", math.huge }, { "-inf", -math.huge }, { "2^31", 2 ^ 31 }, { "2^32", 2 ^ 32 }, { "2^32+1", 2 ^ 32 + 1 }, { "-2^32", -2 ^ 32 }, { "string-1.5", "1.5" }, { "string-spaced", " 1 " }, { "string-hex", "0x1" }, { "false", false }, { "Vector3", Vector3.one }, { "Instance", workspace } } do
	print("name-of-" .. Input[1] .. " " .. Call("GetCollisionGroupName", Input[2]))
end

-- 4. names at the edges
Show("register-default-lower", "RegisterCollisionGroup", "default")
Show("register-DEFAULT", "RegisterCollisionGroup", "DEFAULT")
print("edge-names " .. P.text(table.concat(Names(), ",")))
Clean()
local Umlaut = "\u{00FC}"
Show("register-100-bytes", "RegisterCollisionGroup", string.rep(Umlaut, 50))
Show("register-102-bytes", "RegisterCollisionGroup", string.rep(Umlaut, 51))
Show("register-nul", "RegisterCollisionGroup", "A\0B")
Show("registered-nul", "IsCollisionGroupRegistered", "A\0B")
Show("registered-nul-prefix", "IsCollisionGroupRegistered", "A")
print("byte-names " .. #Names())
Clean()
Show("id-empty", "GetCollisionGroupId", "")
Show("id-long", "GetCollisionGroupId", string.rep("L", 101))
Show("collidable-empty", "CollisionGroupsAreCollidable", "", "Default")
Show("collidable-long", "CollisionGroupsAreCollidable", string.rep("L", 101), "Default")
Show("set-Default-Default", "CollisionGroupSetCollidable", "Default", "Default", false)
print("default-mask " .. P.try(function()
	return PhysicsService:GetRegisteredCollisionGroups()[1].mask
end))
Show("collidable-Default-Default", "CollisionGroupsAreCollidable", "Default", "Default")
Show("set-Default-Default-back", "CollisionGroupSetCollidable", "Default", "Default", true)
Call("RegisterCollisionGroup", "ArgA")
Call("CollisionGroupSetCollidable", "Default", "ArgA", false)
Show("collidable-known-unknown", "CollisionGroupsAreCollidable", "ArgA", "Nope")
Show("collidable-unknown-known", "CollisionGroupsAreCollidable", "Nope", "ArgA")
Show("collidable-unknown-unknown", "CollisionGroupsAreCollidable", "Nope", "Nope2")
Call("CollisionGroupSetCollidable", "Default", "ArgA", true)

-- 5. which error wins
Show("set-empty-known", "CollisionGroupSetCollidable", "", "Default", false)
Show("set-known-empty", "CollisionGroupSetCollidable", "Default", "", false)
Show("set-long-known", "CollisionGroupSetCollidable", string.rep("L", 101), "Default", false)
Show("set-unknown-bad-bool", "CollisionGroupSetCollidable", "Nope", "Default", "no")
Show("set-nil-nil", "CollisionGroupSetCollidable", nil, nil, nil)
Show("set-known-nil-bool", "CollisionGroupSetCollidable", "Default", nil, "no")
Show("rename-unknown-empty", "RenameCollisionGroup", "Nope", "")
Show("rename-unknown-long", "RenameCollisionGroup", "Nope", string.rep("L", 101))
Show("rename-empty-x", "RenameCollisionGroup", "", "ArgX")
Show("rename-long-x", "RenameCollisionGroup", string.rep("L", 101), "ArgX")
Show("rename-Default-empty", "RenameCollisionGroup", "Default", "")
Show("rename-empty-Default", "RenameCollisionGroup", "", "Default")
Show("rename-nil-nil", "RenameCollisionGroup", nil, nil)
Show("rename-nil-empty", "RenameCollisionGroup", nil, "")
Show("rename-empty-nil", "RenameCollisionGroup", "", nil)
Show("unregister-long", "UnregisterCollisionGroup", string.rep("L", 101))
Show("create-existing", "CreateCollisionGroup", "ArgA")
Show("create-Default", "CreateCollisionGroup", "Default")
Show("create-empty", "CreateCollisionGroup", "")
Show("create-long", "CreateCollisionGroup", string.rep("L", 101))
Show("create-nil", "CreateCollisionGroup", nil)
Show("remove-Default", "RemoveCollisionGroup", "Default")
Show("remove-empty", "RemoveCollisionGroup", "")
Show("remove-long", "RemoveCollisionGroup", string.rep("L", 101))
Show("remove-nil", "RemoveCollisionGroup", nil)
print("after-errors " .. P.text(table.concat(Names(), ",")))
Clean()

-- 6. at the limit
for Index = 1, 31 do
	Call("RegisterCollisionGroup", "ArgLimit" .. Index)
end
print("full " .. #Names())
Show("full-existing", "RegisterCollisionGroup", "ArgLimit1")
Show("full-Default", "RegisterCollisionGroup", "Default")
Show("full-empty", "RegisterCollisionGroup", "")
Show("full-long", "RegisterCollisionGroup", string.rep("L", 101))
Show("full-new", "RegisterCollisionGroup", "ArgOver")
Show("full-create-existing", "CreateCollisionGroup", "ArgLimit1")
Show("full-rename", "RenameCollisionGroup", "ArgLimit31", "ArgRenamed")
print("full-names " .. P.text(table.concat(Names(), ",", 30)))
Show("full-name-31", "GetCollisionGroupName", 31)
Show("full-id-renamed", "GetCollisionGroupId", "ArgRenamed")
Clean()

-- 7. the part methods
local Part = Instance.new("Part")
Show("contains-nil", "CollisionGroupContainsPart", "Default", nil)
Show("contains-folder", "CollisionGroupContainsPart", "Default", Instance.new("Folder"))
Show("contains-string", "CollisionGroupContainsPart", "Default", "Part")
Show("contains-default", "CollisionGroupContainsPart", "Default", Part)
Show("contains-unknown-name", "CollisionGroupContainsPart", "Nope", Part)
Show("contains-nil-name", "CollisionGroupContainsPart", nil, Part)
Show("contains-nil-nil", "CollisionGroupContainsPart", nil, nil)
Show("contains-number-name", "CollisionGroupContainsPart", 5, Part)
Show("set-part-nil-nil", "SetPartCollisionGroup", nil, nil)
Show("set-part-string", "SetPartCollisionGroup", "Part", "Default")
Show("set-part-unknown", "SetPartCollisionGroup", Part, "Nope")
print("part-after-unknown " .. P.try(function()
	return Part.CollisionGroup, Part.CollisionGroupId
end))
Show("set-part-empty", "SetPartCollisionGroup", Part, "")
Show("set-part-long", "SetPartCollisionGroup", Part, string.rep("L", 101))
Show("set-part-nil-name", "SetPartCollisionGroup", Part, nil)
Show("set-part-nil", "SetPartCollisionGroup", nil, "Default")
Show("set-part-folder", "SetPartCollisionGroup", Instance.new("Folder"), "Default")
print("part-after-edges " .. P.try(function()
	return Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-long-name " .. P.try(function()
	Part.CollisionGroup = string.rep("L", 101)
	return #Part.CollisionGroup
end))
print("part-unicode-cut " .. P.try(function()
	Part.CollisionGroup = string.rep("L", 99) .. "\u{00FC}"
	return #Part.CollisionGroup
end))
print("part-number-name " .. P.try(function()
	Part.CollisionGroup = 2.5
	return Part.CollisionGroup
end))
print("part-nil-name " .. P.try(function()
	Part.CollisionGroup = nil
	return Part.CollisionGroup
end))
print("part-bool-name " .. P.try(function()
	Part.CollisionGroup = true
	return Part.CollisionGroup
end))

-- 8. a part in the Workspace
Call("RegisterCollisionGroup", "ArgA")
Call("RegisterCollisionGroup", "ArgB")
local Placed = Instance.new("Part")
Placed.Anchored = true
Placed.Parent = workspace
print("placed-default " .. P.try(function()
	return Placed.CollisionGroup, Placed.CollisionGroupId
end))
print("placed-set-B " .. P.try(function()
	Placed.CollisionGroup = "ArgB"
	return Placed.CollisionGroup, Placed.CollisionGroupId
end))
task.wait()
task.wait()
print("placed-set-B-later " .. P.try(function()
	return Placed.CollisionGroup, Placed.CollisionGroupId
end))
print("placed-set-id-1 " .. P.try(function()
	Placed.CollisionGroupId = 1
	return Placed.CollisionGroup, Placed.CollisionGroupId
end))
task.wait()
task.wait()
print("placed-set-id-1-later " .. P.try(function()
	return Placed.CollisionGroup, Placed.CollisionGroupId
end))
print("placed-set-unknown " .. P.try(function()
	Placed.CollisionGroup = "Nope"
	return Placed.CollisionGroup, Placed.CollisionGroupId
end))
task.wait()
task.wait()
print("placed-set-unknown-later " .. P.try(function()
	return Placed.CollisionGroup, Placed.CollisionGroupId
end))
Show("placed-contains-Nope", "CollisionGroupContainsPart", "Nope", Placed)
print("placed-unregister " .. P.try(function()
	Placed.CollisionGroup = "ArgB"
	PhysicsService:UnregisterCollisionGroup("ArgB")
	task.wait()
	task.wait()
	return Placed.CollisionGroup, Placed.CollisionGroupId
end))
print("placed-rename " .. P.try(function()
	Placed.CollisionGroup = "ArgA"
	PhysicsService:RenameCollisionGroup("ArgA", "ArgC")
	task.wait()
	task.wait()
	return Placed.CollisionGroup, Placed.CollisionGroupId
end))
Placed:Destroy()
Clean()

-- 9. parts that come into and leave the Workspace, and groups registered for names parts already hold
Call("RegisterCollisionGroup", "ArgA")
Call("RegisterCollisionGroup", "ArgB")
local Model = Instance.new("Model")
local Inner = Instance.new("Part")
Inner.Anchored = true
Inner.CollisionGroup = "ArgB"
Inner.Parent = Model
local Loose = Instance.new("Part")
Loose.Anchored = true
Loose.CollisionGroup = "ArgB"
print("outside " .. P.try(function()
	return Inner.CollisionGroupId, Loose.CollisionGroupId
end))
Model.Parent = workspace
Loose.Parent = workspace
print("entered " .. P.try(function()
	return Inner.CollisionGroupId, Loose.CollisionGroupId
end))
task.wait()
task.wait()
print("entered-later " .. P.try(function()
	return Inner.CollisionGroupId, Loose.CollisionGroupId
end))
Loose.Parent = nil
print("left " .. P.try(function()
	return Loose.CollisionGroupId
end))
Loose.CollisionGroup = "ArgA"
print("left-set " .. P.try(function()
	return Loose.CollisionGroup, Loose.CollisionGroupId
end))
Loose.CollisionGroupId = 7
Loose.Parent = workspace
print("entered-with-id " .. P.try(function()
	return Loose.CollisionGroup, Loose.CollisionGroupId
end))
Loose.CollisionGroup = "ArgLate"
print("placed-unknown " .. P.try(function()
	return Loose.CollisionGroupId
end))
Show("register-late", "RegisterCollisionGroup", "ArgLate")
print("placed-registered-late " .. P.try(function()
	return Loose.CollisionGroupId
end))
task.wait()
task.wait()
print("placed-registered-late-later " .. P.try(function()
	return Loose.CollisionGroupId
end))
Loose.CollisionGroupId = 9
Show("register-other", "RegisterCollisionGroup", "ArgOther")
print("placed-id-after-register " .. P.try(function()
	return Loose.CollisionGroup, Loose.CollisionGroupId
end))
Show("set-collidable", "CollisionGroupSetCollidable", "ArgOther", "ArgA", false)
print("placed-id-after-collidable " .. P.try(function()
	return Loose.CollisionGroup, Loose.CollisionGroupId
end))
Show("unregister-other", "UnregisterCollisionGroup", "ArgOther")
print("placed-id-after-unregister " .. P.try(function()
	return Loose.CollisionGroup, Loose.CollisionGroupId
end))
print("placed-empty " .. P.try(function()
	Loose.CollisionGroup = ""
	return Loose.CollisionGroup, Loose.CollisionGroupId
end))
print("placed-same " .. P.try(function()
	Loose.CollisionGroupId = 11
	Loose.CollisionGroup = "ArgLate"
	return Loose.CollisionGroup, Loose.CollisionGroupId
end))
print("placed-set-part " .. P.try(function()
	Loose.CollisionGroupId = 11
	PhysicsService:SetPartCollisionGroup(Loose, "ArgA")
	return Loose.CollisionGroup, Loose.CollisionGroupId
end))
Model:Destroy()
Loose:Destroy()
Clean()

-- 10. which of these writes fire Changed: the id worked out on the way in, by a name and by the registry, an ignored, an
-- unchanged and a cut name
Call("RegisterCollisionGroup", "ArgA")
local Watched = Instance.new("Part")
Watched.Anchored = true
local Fired = {}
Watched.Changed:Connect(function(Name)
	if Name == "CollisionGroup" or Name == "CollisionGroupId" then
		table.insert(Fired, Name)
	end
end)
local function Took(Label, Do)
	table.clear(Fired)
	local Outcome = P.try(Do)
	task.wait()
	task.wait()
	print(Label .. " " .. Outcome .. " | " .. table.concat(Fired, ",") .. " | " .. string.sub(Watched.CollisionGroup, 1, 8) .. " " .. #Watched.CollisionGroup .. " " .. Watched.CollisionGroupId)
end
Took("changed-name-outside", function()
	Watched.CollisionGroup = "ArgA"
end)
Took("changed-enter", function()
	Watched.Parent = workspace
end)
Took("changed-name-placed", function()
	Watched.CollisionGroup = "ArgNope"
end)
Took("changed-empty", function()
	Watched.CollisionGroup = ""
end)
Took("changed-same", function()
	Watched.CollisionGroup = "ArgNope"
end)
Took("changed-register", function()
	PhysicsService:RegisterCollisionGroup("ArgNope")
end)
Took("changed-id", function()
	Watched.CollisionGroupId = 5
end)
Took("changed-collidable", function()
	PhysicsService:CollisionGroupSetCollidable("ArgNope", "ArgA", false)
end)
Took("changed-unregister", function()
	PhysicsService:UnregisterCollisionGroup("ArgA")
end)
Took("changed-long", function()
	Watched.CollisionGroup = string.rep("L", 120)
end)
Took("changed-long-again", function()
	Watched.CollisionGroup = string.rep("L", 130)
end)
Took("changed-leave", function()
	Watched.Parent = nil
end)
Watched:Destroy()
Clean()
print("end " .. P.text(table.concat(Names(), ",")))
