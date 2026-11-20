-- parity: loopback
-- T13: events that arrive before anything is connected to them: the server fires a RemoteEvent the client has but does not
-- listen to yet (10 times, then 300 times on another), and the client connects two handlers later; the client fires one the
-- server connects later. Printed: what each handler got, in order, and the warnings the receiving side logged. An invoke of
-- a RemoteFunction whose callback is set only later.
local Storage = game:GetService("ReplicatedStorage")
local LogService = game:GetService("LogService")
local Logged = {}
LogService.MessageOut:Connect(function(Message, Kind)
	if Kind ~= Enum.MessageType.MessageOutput and not string.find(Message, "__Loopback", 1, true) then
		table.insert(Logged, Kind.Name .. " " .. (string.gsub(string.gsub(Message, "\n.*", ""), "%d%d%d+", "N")))
	end
end)
local function Logs()
	local Text = table.concat(Logged, " / ")
	table.clear(Logged)
	return Text
end
local function Range(List)
	if #List == 0 then
		return "none"
	end
	return #List .. " from " .. List[1] .. " to " .. List[#List]
end
--- server
local Player = P.player()
local Few = Instance.new("RemoteEvent")
Few.Name = "Few"
Few.Parent = Storage
local Many = Instance.new("RemoteEvent")
Many.Name = "Many"
Many.Parent = Storage
local Up = Instance.new("RemoteEvent")
Up.Name = "Up"
Up.Parent = Storage
local Call = Instance.new("RemoteFunction")
Call.Name = "Call"
Call.Parent = Storage
P.meet("ready")
for Index = 1, 10 do
	Few:FireClient(Player, Index)
end
for Index = 1, 300 do
	Many:FireClient(Player, Index)
end
P.meet("fired")
P.meet("client fired")
for _ = 1, 30 do
	task.wait()
end
local Got = {}
Up.OnServerEvent:Connect(function(_, Index)
	table.insert(Got, Index)
end)
for _ = 1, 10 do
	task.wait()
end
print("late handler got " .. Range(Got))
print("server logs: " .. Logs())
P.meet("invoking")
for _ = 1, 30 do
	task.wait()
end
Call.OnServerInvoke = function(_, Value)
	return Value * 2
end
P.meet("set")
--- client
local Few = Storage:WaitForChild("Few")
local Many = Storage:WaitForChild("Many")
local Up = Storage:WaitForChild("Up")
local Call = Storage:WaitForChild("Call")
P.meet("ready")
P.meet("fired")
for _ = 1, 30 do
	task.wait()
end
local First, Second, Third = {}, {}, {}
Few.OnClientEvent:Connect(function(Index)
	table.insert(First, Index)
end)
Few.OnClientEvent:Connect(function(Index)
	table.insert(Second, Index)
end)
Many.OnClientEvent:Connect(function(Index)
	table.insert(Third, Index)
end)
for _ = 1, 10 do
	task.wait()
end
print("first handler got " .. Range(First))
print("second handler got " .. Range(Second))
print("many handler got " .. Range(Third))
print("client logs: " .. Logs())
for Index = 1, 10 do
	Up:FireServer(Index)
end
P.meet("client fired")
P.meet("invoking")
local Answer
task.spawn(function()
	Answer = P.try(Call.InvokeServer, Call, 21)
end)
P.meet("set")
for _ = 1, 30 do
	task.wait()
end
print("invoke before the callback " .. tostring(Answer))
