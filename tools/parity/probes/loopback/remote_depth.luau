-- parity: loopback
-- T3/T4: how deep a table sent through a remote may nest, each way: the sender bisects the deepest nesting its FireServer /
-- FireClient accepts (an array of one table in each), then sends that and one level more, and the receiver says how deep
-- what arrived is. Also the error text past it, and how deep a RemoteFunction's returned table may be.
local Storage = game:GetService("ReplicatedStorage")
local function Nest(Depth)
	local Root = {}
	local At = Root
	for _ = 2, Depth do
		local Inner = {}
		At[1] = Inner
		At = Inner
	end
	return Root
end
local function Height(Value)
	local Count = 0
	while type(Value) == "table" do
		Count += 1
		Value = Value[1]
	end
	return Count
end
local function Deepest(Fire)
	local Low, High = 1, 2000
	while Low < High do
		local Middle = (Low + High + 1) // 2
		if pcall(Fire, Nest(Middle)) then
			Low = Middle
		else
			High = Middle - 1
		end
	end
	return Low
end
--- server
local Player = P.player()
local Event = Instance.new("RemoteEvent")
Event.Name = "Depth"
Event.Parent = Storage
local Call = Instance.new("RemoteFunction")
Call.Name = "Call"
Call.Parent = Storage
Call.OnServerInvoke = function(_, Depth)
	return Nest(Depth)
end
local Got = {}
Event.OnServerEvent:Connect(function(_, Value)
	table.insert(Got, Height(Value))
end)
P.meet("ready")
local Deep = Deepest(function(Value)
	Event:FireClient(Player, Value)
end)
print("server deepest send " .. Deep .. ", one more: " .. P.try(Event.FireClient, Event, Player, Nest(Deep + 1)))
P.meet("server sent")
P.meet("client sent")
for _ = 1, 10 do
	task.wait()
end
table.sort(Got)
print("server got heights up to " .. tostring(Got[#Got]) .. " of " .. #Got)
--- client
local Event = Storage:WaitForChild("Depth")
local Call = Storage:WaitForChild("Call")
local Got = {}
Event.OnClientEvent:Connect(function(Value)
	table.insert(Got, Height(Value))
end)
P.meet("ready")
P.meet("server sent")
for _ = 1, 10 do
	task.wait()
end
table.sort(Got)
print("client got heights up to " .. tostring(Got[#Got]) .. " of " .. #Got)
local Deep = Deepest(function(Value)
	Event:FireServer(Value)
end)
print("client deepest send " .. Deep .. ", one more: " .. P.try(Event.FireServer, Event, Nest(Deep + 1)))
print("invoke returning 100 deep " .. P.try(function()
	return Height(Call:InvokeServer(100))
end))
print("invoke returning 1000 deep " .. P.try(function()
	return Height(Call:InvokeServer(1000))
end))
P.meet("client sent")
