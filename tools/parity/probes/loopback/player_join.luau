-- parity: loopback
-- T6: the player's join on each side: PlayerAdded on the server (connected before the join), the player's children there
-- and on the client, what StarterGui, StarterPack and StarterPlayerScripts put into them on each side, and whether a
-- client's PlayerAdded connected after its own player joined replays that join (it must not).
local Players = game:GetService("Players")
local function Children(Instance_)
	local Names = {}
	for _, Child in Instance_:GetChildren() do
		table.insert(Names, Child.ClassName .. ":" .. Child.Name)
	end
	table.sort(Names)
	return table.concat(Names, " ")
end
local function Describe(Player)
	local Parts = {}
	for _, Name in { "PlayerGui", "Backpack", "PlayerScripts" } do
		local Container = Player:FindFirstChild(Name)
		table.insert(Parts, Name .. "=" .. (if Container then "{" .. Children(Container) .. "}" else "none"))
	end
	return table.concat(Parts, " ")
end
--- server
local Gui = Instance.new("ScreenGui")
Gui.Name = "JoinGui"
Gui.ResetOnSpawn = false
Gui.Parent = game:GetService("StarterGui")
local Tool = Instance.new("Tool")
Tool.Name = "JoinTool"
Tool.Parent = game:GetService("StarterPack")
local Scripts = game:GetService("StarterPlayer"):FindFirstChild("StarterPlayerScripts")
print("starter player scripts " .. tostring(Scripts ~= nil))
if Scripts then
	local Folder = Instance.new("Folder")
	Folder.Name = "JoinScripts"
	Folder.Parent = Scripts
end
local Added = {}
Players.PlayerAdded:Connect(function(Player)
	table.insert(Added, "added " .. tostring(Player == Players:GetPlayers()[1]) .. " children {" .. Children(Player) .. "}")
end)
local Player = P.player()
task.wait()
print(table.concat(Added, " / "))
print("name matches " .. tostring(Player.Name ~= "") .. " userid positive " .. tostring(Player.UserId ~= 0))
P.meet("joined")
for _ = 1, 30 do
	task.wait()
end
print("children {" .. Children(Player) .. "}")
print(Describe(Player))
print("players " .. #Players:GetPlayers())
P.meet("seen")
--- client
local Count = 0
Players.PlayerAdded:Connect(function()
	Count += 1
end)
local Player = Players.LocalPlayer
P.meet("joined")
for _ = 1, 30 do
	task.wait()
end
print("children {" .. Children(Player) .. "}")
print(Describe(Player))
print("players " .. #Players:GetPlayers() .. " late PlayerAdded fired " .. Count)
P.meet("seen")
