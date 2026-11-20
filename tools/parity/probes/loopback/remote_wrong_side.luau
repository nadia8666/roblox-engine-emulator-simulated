-- parity: loopback
-- T11: the calls each side may not make on remotes, with their errors: FireClient, FireAllClients and InvokeClient on a
-- client, FireServer and InvokeServer on a server; connecting the other side's event, setting the other side's callback,
-- and reading those members.
local Storage = game:GetService("ReplicatedStorage")
local function Nothing() end
local function Report(Event, Function, Player)
	print("FireServer " .. P.try(Event.FireServer, Event, 1))
	print("FireClient " .. P.try(Event.FireClient, Event, Player, 1))
	print("FireClient no player " .. P.try(Event.FireClient, Event, workspace, 1))
	print("FireAllClients " .. P.try(Event.FireAllClients, Event, 1))
	print("read OnServerEvent " .. P.try(function()
		return typeof(Event.OnServerEvent)
	end))
	print("connect OnServerEvent " .. P.try(function()
		local Connection = Event.OnServerEvent:Connect(Nothing)
		Connection:Disconnect()
		return true
	end))
	print("connect OnClientEvent " .. P.try(function()
		local Connection = Event.OnClientEvent:Connect(Nothing)
		Connection:Disconnect()
		return true
	end))
	print("set OnServerInvoke " .. P.try(function()
		Function.OnServerInvoke = Nothing
	end))
	print("set OnClientInvoke " .. P.try(function()
		Function.OnClientInvoke = Nothing
	end))
	print("read OnServerInvoke " .. P.try(function()
		return Function.OnServerInvoke
	end))
	print("read OnClientInvoke " .. P.try(function()
		return Function.OnClientInvoke
	end))
	print("InvokeClient no player " .. P.try(Function.InvokeClient, Function, workspace))
	local Loose = Instance.new("UnreliableRemoteEvent")
	print("parentless unreliable FireServer " .. P.try(Loose.FireServer, Loose, 1) .. " FireAllClients " .. P.try(Loose.FireAllClients, Loose, 1))
	local Unreliable = Storage:WaitForChild("Unreliable")
	print("unreliable FireServer " .. P.try(Unreliable.FireServer, Unreliable, 1) .. " FireClient " .. P.try(Unreliable.FireClient, Unreliable, Player, 1)
		.. " FireAllClients " .. P.try(Unreliable.FireAllClients, Unreliable, 1) .. " connect OnServerEvent " .. P.try(function()
			Unreliable.OnServerEvent:Connect(Nothing):Disconnect()
		end) .. " connect OnClientEvent " .. P.try(function()
			Unreliable.OnClientEvent:Connect(Nothing):Disconnect()
		end))
	local LooseEvent = Instance.new("RemoteEvent")
	print("parentless FireServer " .. P.try(LooseEvent.FireServer, LooseEvent, 1) .. " FireAllClients " .. P.try(LooseEvent.FireAllClients, LooseEvent, 1))
end
--- server
local Player = P.player()
local Event = Instance.new("RemoteEvent")
Event.Name = "Event"
Event.Parent = Storage
local Function = Instance.new("RemoteFunction")
Function.Name = "Function"
Function.Parent = Storage
local Unreliable = Instance.new("UnreliableRemoteEvent")
Unreliable.Name = "Unreliable"
Unreliable.Parent = Storage
P.meet("ready")
Report(Event, Function, Player)
print("InvokeServer " .. P.try(Function.InvokeServer, Function, 1))
--- client
local Event = Storage:WaitForChild("Event")
local Function = Storage:WaitForChild("Function")
P.meet("ready")
Report(Event, Function, P.player())
-- (InvokeClient on a client: run on a thread of its own, in case it waits)
local Thread = task.spawn(function()
	print("InvokeClient " .. P.try(Function.InvokeClient, Function, P.player(), 1))
end)
for _ = 1, 5 do
	task.wait()
end
if coroutine.status(Thread) ~= "dead" then
	print("InvokeClient still waiting")
end
