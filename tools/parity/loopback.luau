-- Prepended (after prelude.luau) to each side of a paired probe (probes/loopback/<name>.luau), in ree's loopback run and in
-- the real engine's play session alike: a server Script and the client's job run the same probe file, each with the other
-- side's section blanked out. These helpers give the two sides a meeting point and bring the server's lines to the client,
-- which prints both sides' lines (`server: ...`, then `client: ...`).
--
-- The server's half starts with the server, before the player joins: it makes the meeting remote and the lines' value in
-- ReplicatedStorage first, so they reach the client with the join. The client's half starts once the client runs scripts.
local __LoopbackPlayers = game:GetService("Players")
local __LoopbackStorage = game:GetService("ReplicatedStorage")
local __LoopbackRunService = game:GetService("RunService")
P.side = if __LoopbackRunService:IsServer() then "server" else "client"

local __LoopbackSync, __LoopbackOut
local __LoopbackArrived = {}
if P.side == "server" then
	__LoopbackSync = Instance.new("RemoteEvent")
	__LoopbackSync.Name = "__LoopbackSync"
	__LoopbackSync.OnServerEvent:Connect(function(_, Tag)
		__LoopbackArrived[Tag] = true
	end)
	__LoopbackSync.Parent = __LoopbackStorage
	__LoopbackOut = Instance.new("StringValue")
	__LoopbackOut.Name = "__LoopbackLines"
	__LoopbackOut.Parent = __LoopbackStorage
else
	__LoopbackSync = __LoopbackStorage:WaitForChild("__LoopbackSync", 30)
	if __LoopbackSync then
		__LoopbackSync.OnClientEvent:Connect(function(Tag)
			__LoopbackArrived[Tag] = true
		end)
	end
end

-- The player: the client's own, or the first to join the server (waited for)
function P.player()
	if P.side == "client" then
		return __LoopbackPlayers.LocalPlayer
	end
	local Player = __LoopbackPlayers:GetPlayers()[1]
	while not Player do
		__LoopbackPlayers.PlayerAdded:Wait()
		Player = __LoopbackPlayers:GetPlayers()[1]
	end
	return Player
end

-- Both sides at the same point: each returns once the other has reached P.meet(Tag) too, at a resumption point (the frames
-- counted from here are comparable within a side, never across sides: the two run their own frames)
function P.meet(Tag)
	assert(__LoopbackSync, "no meeting remote")
	local Deadline = os.clock() + 60
	if P.side == "client" then
		__LoopbackSync:FireServer(Tag)
		while not __LoopbackArrived[Tag] and os.clock() < Deadline do
			task.wait()
		end
	else
		while not __LoopbackArrived[Tag] and os.clock() < Deadline do
			task.wait()
		end
		__LoopbackSync:FireClient(P.player(), Tag)
	end
	assert(__LoopbackArrived[Tag], "the other side never met at " .. tostring(Tag))
	P.sync()
end

-- The end of a side: the server publishes its lines; the client waits for them and prints both sides' lines with `print`
function __LoopbackFinish(Lines, Print)
	if P.side == "server" then
		__LoopbackOut.Value = if #Lines == 0 then "\n" else table.concat(Lines, "\n")
		return
	end
	local Out = __LoopbackStorage:WaitForChild("__LoopbackLines", 30)
	local Deadline = os.clock() + 90
	while Out and Out.Value == "" and os.clock() < Deadline do
		task.wait()
	end
	if not Out or Out.Value == "" then
		Print("server: (no lines)")
	elseif Out.Value ~= "\n" then
		for _, Line in string.split(Out.Value, "\n") do
			Print("server: " .. Line)
		end
	end
	for _, Line in Lines do
		Print("client: " .. Line)
	end
end
