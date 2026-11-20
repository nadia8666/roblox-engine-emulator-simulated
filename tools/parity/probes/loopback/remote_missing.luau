-- parity: loopback
-- T12: firing a remote the other side does not have: a RemoteEvent the client made itself (FireServer), one the server keeps
-- in ServerStorage (FireClient), one the server destroys or unparents right after firing it, and an InvokeServer of a
-- client-made RemoteFunction. Printed: what arrived, and the warnings and errors each side's LogService saw.
local Storage = game:GetService("ReplicatedStorage")
local LogService = game:GetService("LogService")
local Logged = {}
LogService.MessageOut:Connect(function(Message, Kind)
	if Kind ~= Enum.MessageType.MessageOutput and not string.find(Message, "__Loopback", 1, true) then
		table.insert(Logged, Kind.Name .. " " .. (string.gsub(Message, "\n.*", "")))
	end
end)
local function Logs()
	local Text = table.concat(Logged, " / ")
	table.clear(Logged)
	return Text
end
--- server
local Player = P.player()
local Hidden = Instance.new("RemoteEvent")
Hidden.Name = "Hidden"
Hidden.Parent = game:GetService("ServerStorage")
local Doomed = Instance.new("RemoteEvent")
Doomed.Name = "Doomed"
Doomed.Parent = Storage
local Moved = Instance.new("RemoteEvent")
Moved.Name = "Moved"
Moved.Parent = Storage
P.meet("ready")
print("fire hidden " .. P.try(Hidden.FireClient, Hidden, Player, 1))
Doomed:FireClient(Player, "doomed")
Doomed:Destroy()
Moved:FireClient(Player, "moved")
Moved.Parent = nil
P.meet("fired")
P.meet("client fired")
for _ = 1, 10 do
	task.wait()
end
print("server logs: " .. Logs())
--- client
local Doomed = Storage:WaitForChild("Doomed")
local Moved = Storage:WaitForChild("Moved")
local Got = {}
Doomed.OnClientEvent:Connect(function(Tag)
	table.insert(Got, Tag)
end)
Moved.OnClientEvent:Connect(function(Tag)
	table.insert(Got, Tag)
end)
P.meet("ready")
P.meet("fired")
for _ = 1, 10 do
	task.wait()
end
print("client got " .. table.concat(Got, " ") .. " doomed parent " .. tostring(Doomed.Parent) .. " moved parent " .. tostring(Moved.Parent))
local Own = Instance.new("RemoteEvent")
Own.Name = "ClientMade"
Own.Parent = Storage
print("fire client-made " .. P.try(Own.FireServer, Own, 1))
local Function = Instance.new("RemoteFunction")
Function.Name = "ClientMadeFunction"
Function.Parent = Storage
local Thread = task.spawn(function()
	print("invoke client-made " .. P.try(Function.InvokeServer, Function, 1))
end)
for _ = 1, 30 do
	task.wait()
end
if coroutine.status(Thread) ~= "dead" then
	print("invoke client-made still waiting")
end
P.meet("client fired")
print("client logs: " .. Logs())
