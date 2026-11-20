-- PhysicsService's collision group registry: the Default group a run starts with, registering, renaming and
-- unregistering groups, the collidable pairs (GetRegisteredCollisionGroups' masks), the group limit, the argument
-- errors, the deprecated methods, and BasePart.CollisionGroup / CollisionGroupId with registered and unknown names.
-- The probe removes every group it makes, so the session's later probes see the registry as it found it.
local PhysicsService = game:GetService("PhysicsService")

local function Value(Item)
	if type(Item) == "number" then
		return P.num(Item)
	end
	return (string.gsub(tostring(Item), "0x%x+", "ADDR"))
end

-- GetRegisteredCollisionGroups (or another method returning the same shape) as one line: the count, then each entry's
-- keys sorted, with their values and types
local function Groups(Method)
	local Ok, List = pcall(PhysicsService[Method or "GetRegisteredCollisionGroups"], PhysicsService)
	if not Ok then
		return "error " .. P.err(List)
	end
	if type(List) ~= "table" then
		return typeof(List) .. " " .. Value(List)
	end
	local Parts = {}
	for Index, Entry in List do
		local Keys = {}
		if type(Entry) == "table" then
			for Key, Item in Entry do
				table.insert(Keys, tostring(Key) .. "=" .. Value(Item) .. ":" .. typeof(Item))
			end
			table.sort(Keys)
			table.insert(Parts, Value(Index) .. "{" .. table.concat(Keys, ",") .. "}")
		else
			table.insert(Parts, Value(Index) .. "=" .. Value(Entry) .. ":" .. typeof(Entry))
		end
	end
	return tostring(#List) .. " " .. table.concat(Parts, " ")
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

local Changes = {}
local ChangedOk, ChangedConnection = pcall(function()
	return PhysicsService.CollisionGroupCollidableChanged:Connect(function(...)
		local Parts = table.pack(...)
		for Index = 1, Parts.n do
			Parts[Index] = Value(Parts[Index]) .. ":" .. typeof(Parts[Index])
		end
		table.insert(Changes, table.concat(Parts, ","))
	end)
end)
print("changed-connect " .. tostring(ChangedOk) .. (if ChangedOk then "" else " " .. P.err(ChangedConnection)))
local function FlushChanges(Label)
	task.wait()
	task.wait()
	print(Label .. " changes " .. #Changes .. " " .. table.concat(Changes, " "))
	table.clear(Changes)
end

-- 1. the start
print("start " .. Groups())
Show("max", "GetMaxCollisionGroups")
Show("registered-Default", "IsCollisionGroupRegistered", "Default")
Show("registered-default", "IsCollisionGroupRegistered", "default")
Show("registered-Nope", "IsCollisionGroupRegistered", "Nope")
Show("registered-empty", "IsCollisionGroupRegistered", "")
Show("registered-nil", "IsCollisionGroupRegistered", nil)
Show("registered-none", "IsCollisionGroupRegistered")
Show("registered-number", "IsCollisionGroupRegistered", 5)
Show("registered-table", "IsCollisionGroupRegistered", {})
Show("collidable-Default", "CollisionGroupsAreCollidable", "Default", "Default")
Show("collidable-unknown", "CollisionGroupsAreCollidable", "Default", "Nope")
Show("collidable-unknown2", "CollisionGroupsAreCollidable", "Nope", "Default")
Show("collidable-nil", "CollisionGroupsAreCollidable", "Default", nil)

-- 2. registering
Show("register-A", "RegisterCollisionGroup", "ProbeA")
print("after-A " .. Groups())
Show("registered-A", "IsCollisionGroupRegistered", "ProbeA")
Show("registered-a", "IsCollisionGroupRegistered", "probea")
Show("register-A-again", "RegisterCollisionGroup", "ProbeA")
Show("register-a-case", "RegisterCollisionGroup", "probea")
print("after-case " .. Groups())
Show("unregister-a-case", "UnregisterCollisionGroup", "probea")
Show("register-Default", "RegisterCollisionGroup", "Default")
Show("register-empty", "RegisterCollisionGroup", "")
Show("register-spaces", "RegisterCollisionGroup", "   ")
Show("registered-spaces", "IsCollisionGroupRegistered", "   ")
Show("unregister-spaces", "UnregisterCollisionGroup", "   ")
Show("register-nil", "RegisterCollisionGroup", nil)
Show("register-none", "RegisterCollisionGroup")
Show("register-table", "RegisterCollisionGroup", {})
Show("register-bool", "RegisterCollisionGroup", true)
Show("register-number", "RegisterCollisionGroup", 5)
Show("registered-5", "IsCollisionGroupRegistered", "5")
Show("unregister-5", "UnregisterCollisionGroup", "5")
Show("register-unicode", "RegisterCollisionGroup", "Gr\u{00FC}ppe")
Show("registered-unicode", "IsCollisionGroupRegistered", "Gr\u{00FC}ppe")
Show("unregister-unicode", "UnregisterCollisionGroup", "Gr\u{00FC}ppe")
for _, Length in { 32, 63, 64, 65, 99, 100, 101, 127, 128, 129, 255, 256, 1000 } do
	local Name = string.rep("L", Length)
	local Outcome = Call("RegisterCollisionGroup", Name)
	local Registered = Call("IsCollisionGroupRegistered", Name)
	print("register-length-" .. Length .. " " .. Outcome .. " | " .. Registered)
	if Registered == "ok true" then
		Call("UnregisterCollisionGroup", Name)
	end
end
print("after-names " .. Groups())

-- 3. collidable pairs
Show("register-B", "RegisterCollisionGroup", "ProbeB")
Show("register-C", "RegisterCollisionGroup", "ProbeC")
print("after-BC " .. Groups())
Show("set-A-B-false", "CollisionGroupSetCollidable", "ProbeA", "ProbeB", false)
print("after-AB " .. Groups())
Show("collidable-A-B", "CollisionGroupsAreCollidable", "ProbeA", "ProbeB")
Show("collidable-B-A", "CollisionGroupsAreCollidable", "ProbeB", "ProbeA")
Show("collidable-A-C", "CollisionGroupsAreCollidable", "ProbeA", "ProbeC")
FlushChanges("set-A-B-false")
Show("set-A-B-false-again", "CollisionGroupSetCollidable", "ProbeA", "ProbeB", false)
FlushChanges("set-A-B-false-again")
Show("set-C-C-false", "CollisionGroupSetCollidable", "ProbeC", "ProbeC", false)
Show("collidable-C-C", "CollisionGroupsAreCollidable", "ProbeC", "ProbeC")
Show("set-Default-C-false", "CollisionGroupSetCollidable", "Default", "ProbeC", false)
print("after-C " .. Groups())
FlushChanges("set-C")
Show("set-B-A-true", "CollisionGroupSetCollidable", "ProbeB", "ProbeA", true)
Show("collidable-A-B-2", "CollisionGroupsAreCollidable", "ProbeA", "ProbeB")
FlushChanges("set-B-A-true")
Show("set-unknown", "CollisionGroupSetCollidable", "ProbeA", "Nope", false)
Show("set-unknown2", "CollisionGroupSetCollidable", "Nope", "ProbeA", false)
Show("set-nil-bool", "CollisionGroupSetCollidable", "ProbeA", "ProbeB", nil)
Show("set-none-bool", "CollisionGroupSetCollidable", "ProbeA", "ProbeB")
Show("set-string-bool", "CollisionGroupSetCollidable", "ProbeA", "ProbeB", "no")
Show("set-number-bool", "CollisionGroupSetCollidable", "ProbeA", "ProbeB", 0)
Show("collidable-A-B-3", "CollisionGroupsAreCollidable", "ProbeA", "ProbeB")
Show("set-nil-name", "CollisionGroupSetCollidable", nil, "ProbeB", false)
Show("set-number-name", "CollisionGroupSetCollidable", 5, "ProbeB", false)
print("after-set " .. Groups())
FlushChanges("set-rest")

-- 4. renaming
Show("rename-A-D", "RenameCollisionGroup", "ProbeA", "ProbeD")
print("after-rename " .. Groups())
Show("registered-A-renamed", "IsCollisionGroupRegistered", "ProbeA")
Show("collidable-D-B", "CollisionGroupsAreCollidable", "ProbeD", "ProbeB")
Show("rename-unknown", "RenameCollisionGroup", "Nope", "ProbeE")
Show("rename-to-existing", "RenameCollisionGroup", "ProbeD", "ProbeB")
Show("rename-to-self", "RenameCollisionGroup", "ProbeD", "ProbeD")
Show("rename-to-empty", "RenameCollisionGroup", "ProbeD", "")
Show("rename-to-long", "RenameCollisionGroup", "ProbeD", string.rep("L", 1000))
Show("rename-Default", "RenameCollisionGroup", "Default", "ProbeE")
Show("rename-to-Default", "RenameCollisionGroup", "ProbeD", "Default")
Show("rename-nil", "RenameCollisionGroup", "ProbeD", nil)
Show("rename-D-A", "RenameCollisionGroup", "ProbeD", "ProbeA")
print("after-renames " .. Groups())
FlushChanges("rename")

-- 5. unregistering and slot reuse
Show("set-B-C-false", "CollisionGroupSetCollidable", "ProbeB", "ProbeC", false)
Show("unregister-B", "UnregisterCollisionGroup", "ProbeB")
print("after-unregister-B " .. Groups())
Show("registered-B", "IsCollisionGroupRegistered", "ProbeB")
Show("collidable-B-gone", "CollisionGroupsAreCollidable", "ProbeA", "ProbeB")
Show("unregister-B-again", "UnregisterCollisionGroup", "ProbeB")
Show("unregister-unknown", "UnregisterCollisionGroup", "Nope")
Show("unregister-empty", "UnregisterCollisionGroup", "")
Show("unregister-nil", "UnregisterCollisionGroup", nil)
Show("unregister-Default", "UnregisterCollisionGroup", "Default")
Show("register-F", "RegisterCollisionGroup", "ProbeF")
print("after-F " .. Groups())
Show("collidable-F-C", "CollisionGroupsAreCollidable", "ProbeF", "ProbeC")
Show("register-B-back", "RegisterCollisionGroup", "ProbeB")
print("after-B-back " .. Groups())
FlushChanges("unregister")

-- 6. the result is a copy
local Ok, List = pcall(PhysicsService.GetRegisteredCollisionGroups, PhysicsService)
if Ok and type(List) == "table" then
	print("frozen " .. tostring(table.isfrozen(List)) .. " " .. tostring(type(List[1]) == "table" and table.isfrozen(List[1])))
	print("same-table " .. tostring(rawequal(List, PhysicsService:GetRegisteredCollisionGroups())))
	if type(List[1]) == "table" then
		List[1].name = "Changed"
		List[1].mask = 0
	end
	table.insert(List, { name = "Inserted", mask = 1 })
	print("after-mutate " .. Groups())
end

-- 7. BasePart.CollisionGroup and CollisionGroupId
local Part = Instance.new("Part")
print("part-default " .. P.try(function()
	return Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-set-A " .. P.try(function()
	Part.CollisionGroup = "ProbeA"
	return Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-set-unknown " .. P.try(function()
	Part.CollisionGroup = "NotRegistered"
	return Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-set-empty " .. P.try(function()
	Part.CollisionGroup = ""
	return Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-set-number " .. P.try(function()
	Part.CollisionGroup = 5
	return Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-set-id-2 " .. P.try(function()
	Part.CollisionGroupId = 2
	return Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-set-id-30 " .. P.try(function()
	Part.CollisionGroupId = 30
	return Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-set-id-neg " .. P.try(function()
	Part.CollisionGroupId = -1
	return Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-set-C " .. P.try(function()
	Part.CollisionGroup = "ProbeC"
	return Part.CollisionGroup, Part.CollisionGroupId
end))
Show("contains-C", "CollisionGroupContainsPart", "ProbeC", Part)
Show("contains-A", "CollisionGroupContainsPart", "ProbeA", Part)
Show("set-part-A", "SetPartCollisionGroup", Part, "ProbeA")
print("part-after-set-part " .. P.try(function()
	return Part.CollisionGroup, Part.CollisionGroupId
end))
Show("rename-C-G", "RenameCollisionGroup", "ProbeC", "ProbeG")
print("part-after-rename " .. P.try(function()
	Part.CollisionGroup = "ProbeG"
	local Before = Part.CollisionGroup
	PhysicsService:RenameCollisionGroup("ProbeG", "ProbeC")
	return Before, Part.CollisionGroup, Part.CollisionGroupId
end))
print("part-after-unregister " .. P.try(function()
	PhysicsService:UnregisterCollisionGroup("ProbeC")
	return Part.CollisionGroup, Part.CollisionGroupId
end))
Part:Destroy()

-- 8. the deprecated methods
Show("create-H", "CreateCollisionGroup", "ProbeH")
print("after-create " .. Groups())
print("get-groups " .. Groups("GetCollisionGroups"))
Show("id-Default", "GetCollisionGroupId", "Default")
Show("id-A", "GetCollisionGroupId", "ProbeA")
Show("id-unknown", "GetCollisionGroupId", "Nope")
Show("name-0", "GetCollisionGroupName", 0)
Show("name-1", "GetCollisionGroupName", 1)
Show("name-31", "GetCollisionGroupName", 31)
Show("name-32", "GetCollisionGroupName", 32)
Show("name-neg", "GetCollisionGroupName", -1)
Show("remove-H", "RemoveCollisionGroup", "ProbeH")
Show("remove-unknown", "RemoveCollisionGroup", "Nope")
print("after-remove " .. Groups())

-- 9. the limit
local Made = {}
local LastError = "none"
for Index = 1, 40 do
	local Name = "ProbeLimit" .. Index
	local Outcome = Call("RegisterCollisionGroup", Name)
	if Outcome == "ok " then
		table.insert(Made, Name)
	else
		LastError = Outcome
		break
	end
end
print("limit made " .. #Made .. " then " .. LastError)
print("limit-groups " .. Groups())
Show("limit-create", "CreateCollisionGroup", "ProbeLimitCreate")
Show("limit-rename", "RenameCollisionGroup", "ProbeA", "ProbeRenamedAtLimit")
for _, Name in Made do
	Call("UnregisterCollisionGroup", Name)
end
FlushChanges("limit")

-- 10. clean up: every group but Default
pcall(function()
	for _, Entry in PhysicsService:GetRegisteredCollisionGroups() do
		if Entry.name ~= "Default" then
			PhysicsService:UnregisterCollisionGroup(Entry.name)
		end
	end
end)
print("end " .. Groups())
if ChangedOk then
	ChangedConnection:Disconnect()
end
