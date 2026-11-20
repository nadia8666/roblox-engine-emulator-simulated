-- parity: loopback
-- T15: what RunService and Players answer on each side of one play session: the Is* answers, LocalPlayer, and the
-- players each side lists once the client has joined.
local RunService = game:GetService("RunService")
local Players = game:GetService("Players")
local function Sides()
	local Parts = {}
	for _, Name in { "IsClient", "IsServer", "IsStudio", "IsRunning", "IsRunMode" } do
		table.insert(Parts, Name .. "=" .. tostring(RunService[Name](RunService)))
	end
	return table.concat(Parts, " ")
end
--- server
local Player = P.player()
P.meet("joined")
print("sides " .. Sides())
print("local-player " .. tostring(Players.LocalPlayer))
print("players " .. #Players:GetPlayers() .. " first-is-joined " .. tostring(Players:GetPlayers()[1] == Player))
--- client
P.meet("joined")
print("sides " .. Sides())
print("local-player " .. tostring(Players.LocalPlayer ~= nil) .. " " .. tostring(Players.LocalPlayer == P.player()))
print("players " .. #Players:GetPlayers() .. " first-is-local " .. tostring(Players:GetPlayers()[1] == Players.LocalPlayer))
