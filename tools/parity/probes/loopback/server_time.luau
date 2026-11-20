-- parity: loopback
-- T16: the clocks the two sides share: the server sends its workspace:GetServerTimeNow(), time() and DistributedGameTime from
-- a Heartbeat, and the client compares them with its own when they arrive.
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
--- server
local Player = P.player()
local Clock = Instance.new("RemoteEvent")
Clock.Name = "Clock"
Clock.Parent = Storage
P.meet("ready")
RunService.Heartbeat:Once(function()
	Clock:FireClient(Player, workspace:GetServerTimeNow(), time(), workspace.DistributedGameTime)
end)
P.meet("sent")
--- client
local Clock = Storage:WaitForChild("Clock")
local Lines = {}
Clock.OnClientEvent:Connect(function(Server, Time, Distributed)
	local Now = workspace:GetServerTimeNow()
	table.insert(Lines, "server time now: client minus server within a second " .. tostring(math.abs(Now - Server) < 1)
		.. ", not behind " .. tostring(Now >= Server))
	table.insert(Lines, "time(): client below the server's " .. tostring(time() < Time))
	table.insert(Lines, "DistributedGameTime: within a second of the server's " .. tostring(math.abs(workspace.DistributedGameTime - Distributed) < 1))
end)
P.meet("ready")
P.meet("sent")
for _ = 1, 10 do
	task.wait()
end
for _, Line in Lines do
	print(Line)
end
