-- parity: loopback
-- What a client sees of the collision groups its server registers: a group registered before the player joins, one
-- registered while it plays, a collidable pair, a rename and an unregister. Each change has 30 frames to reach the
-- client before it reads, and the server changes nothing while the client reads.
local PhysicsService = game:GetService("PhysicsService")
local function Groups()
	local Ok, List = pcall(PhysicsService.GetRegisteredCollisionGroups, PhysicsService)
	if not Ok then
		return "error " .. P.err(List)
	elseif type(List) ~= "table" then
		return typeof(List)
	end
	local Parts = {}
	for _, Entry in List do
		table.insert(Parts, tostring(Entry.name) .. "=" .. P.num(Entry.mask))
	end
	return #List .. " " .. table.concat(Parts, " ")
end
local function Settle()
	for _ = 1, 30 do
		task.wait()
	end
end
--- server
PhysicsService:RegisterCollisionGroup("LoopEarly")
PhysicsService:CollisionGroupSetCollidable("LoopEarly", "Default", false)
print("boot " .. Groups())
P.player()
P.meet("joined")
P.meet("read-join")
PhysicsService:RegisterCollisionGroup("LoopLate")
PhysicsService:CollisionGroupSetCollidable("LoopLate", "LoopEarly", false)
Settle()
P.meet("late")
P.meet("read-late")
PhysicsService:RenameCollisionGroup("LoopEarly", "LoopRenamed")
PhysicsService:UnregisterCollisionGroup("LoopLate")
Settle()
P.meet("changed")
P.meet("read-changed")
print("end " .. Groups())
PhysicsService:UnregisterCollisionGroup("LoopRenamed")
--- client
P.meet("joined")
print("join " .. Groups())
P.meet("read-join")
P.meet("late")
print("late " .. Groups())
print("late-collidable " .. P.try(function()
	return PhysicsService:CollisionGroupsAreCollidable("LoopLate", "LoopEarly")
end))
P.meet("read-late")
P.meet("changed")
print("changed " .. Groups())
P.meet("read-changed")
