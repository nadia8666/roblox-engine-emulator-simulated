-- parity: loopback
-- T8: the order of what the server sends in one frame: an instance made, written, parented and passed to FireClient arrives
-- with its values; a property set before FireClient is seen by the handler and one set after it too or not; two remotes
-- keep their order; a RemoteEvent and an UnreliableRemoteEvent fired in turn; and the client's FireServer calls in order.
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
--- server
local Player = P.player()
local A = Instance.new("RemoteEvent")
A.Name = "A"
A.Parent = Storage
local B = Instance.new("RemoteEvent")
B.Name = "B"
B.Parent = Storage
local U = Instance.new("UnreliableRemoteEvent")
U.Name = "U"
U.Parent = Storage
local Value = Instance.new("IntValue")
Value.Name = "Value"
Value.Parent = Storage
local Order = {}
A.OnServerEvent:Connect(function(_, Tag)
	table.insert(Order, "A" .. Tag)
end)
B.OnServerEvent:Connect(function(_, Tag)
	table.insert(Order, "B" .. Tag)
end)
U.OnServerEvent:Connect(function(_, Tag)
	table.insert(Order, "U" .. Tag)
end)
P.meet("ready")
-- one frame, from a Heartbeat handler
RunService.Heartbeat:Once(function()
	local Part = Instance.new("Part")
	Part.Name = "Made"
	Part.Transparency = 0.5
	Part.Parent = workspace
	Part:SetAttribute("Mark", 7)
	Value.Value = 1
	A:FireClient(Player, "1", Part, Value.Value)
	Value.Value = 2
	B:FireClient(Player, "2")
	A:FireClient(Player, "3")
	U:FireClient(Player, "4")
	A:FireClient(Player, "5")
	U:FireClient(Player, "6")
	B:FireClient(Player, "7")
	Part.Transparency = 0.25
end)
P.meet("sent")
P.meet("client sent")
for _ = 1, 10 do
	task.wait()
end
print("server got " .. table.concat(Order, " "))
--- client
local A = Storage:WaitForChild("A")
local B = Storage:WaitForChild("B")
local U = Storage:WaitForChild("U")
local Value = Storage:WaitForChild("Value")
local Order = {}
A.OnClientEvent:Connect(function(Tag, Part, Sent)
	table.insert(Order, "A" .. Tag)
	if Tag == "1" then
		print("made arrives " .. tostring(Part ~= nil) .. " parent " .. tostring(Part and Part.Parent) .. " transparency "
			.. tostring(Part and P.num(Part.Transparency)) .. " mark " .. tostring(Part and Part:GetAttribute("Mark"))
			.. " value " .. P.num(Value.Value) .. " sent " .. P.num(Sent))
	end
end)
B.OnClientEvent:Connect(function(Tag)
	table.insert(Order, "B" .. Tag)
end)
U.OnClientEvent:Connect(function(Tag)
	table.insert(Order, "U" .. Tag)
end)
local Changes = {}
Value.Changed:Connect(function(New)
	table.insert(Changes, P.num(New))
	table.insert(Order, "Changed" .. P.num(New))
end)
P.meet("ready")
P.meet("sent")
for _ = 1, 10 do
	task.wait()
end
print("client got " .. table.concat(Order, " "))
RunService.Heartbeat:Once(function()
	A:FireServer("1")
	B:FireServer("2")
	U:FireServer("3")
	A:FireServer("4")
	U:FireServer("5")
	B:FireServer("6")
end)
P.meet("client sent")
