-- parity: loopback
-- T5: which services' children reach the client: the server puts a marker Folder into every service it can parent to,
-- one before the player joins and one after, and the client lists the services where each arrived. Also what the client
-- sees of the services that never replicate (GetService, FindService, their children).
local SERVICES = {
	"Workspace", "ReplicatedStorage", "ReplicatedFirst", "Lighting", "Players", "StarterGui", "StarterPack", "StarterPlayer",
	"Teams", "SoundService", "Chat", "TextChatService", "LocalizationService", "MaterialService", "ServerStorage",
	"ServerScriptService", "TestService", "Debris", "HttpService", "CollectionService", "TweenService", "RunService",
	"InsertService", "PhysicsService", "VRService", "GroupService", "BadgeService", "MarketplaceService", "TeleportService",
	"ProximityPromptService", "AvatarEditorService", "SocialService", "PolicyService", "LogService", "AssetService",
	"UserInputService", "GuiService", "ContextActionService", "HapticService", "StarterPlayer.StarterPlayerScripts",
	"StarterPlayer.StarterCharacterScripts",
}
local function Find(Path)
	local Parts = string.split(Path, ".")
	local Found = game:FindService(Parts[1])
	for Index = 2, #Parts do
		Found = Found and Found:FindFirstChild(Parts[Index])
	end
	return Found
end
--- server
local Placed = {}
local function Place(Name)
	local List = {}
	for _, Path in SERVICES do
		local Service = Find(Path) or select(2, pcall(game.GetService, game, Path))
		local Marker = Instance.new("Folder")
		Marker.Name = Name
		if typeof(Service) == "Instance" and pcall(function()
			Marker.Parent = Service
		end) then
			table.insert(List, Path)
		end
	end
	return List
end
local Before = Place("MarkerBeforeJoin")
P.player()
P.meet("joined")
local After = Place("MarkerAfterJoin")
print("placed before " .. table.concat(Before, " "))
print("placed after " .. table.concat(After, " "))
P.meet("placed")
--- client
P.meet("joined")
P.meet("placed")
for _ = 1, 30 do
	task.wait()
end
for _, Name in { "MarkerBeforeJoin", "MarkerAfterJoin" } do
	local List = {}
	for _, Path in SERVICES do
		local Service = Find(Path)
		if Service and Service:FindFirstChild(Name) then
			table.insert(List, Path)
		end
	end
	print(Name .. " arrived in " .. table.concat(List, " "))
end
for _, Name in { "ServerStorage", "ServerScriptService" } do
	local Found = game:FindService(Name)
	print(Name .. " find " .. tostring(Found ~= nil) .. " get " .. P.try(function()
		local Service = game:GetService(Name)
		return Service.ClassName, #Service:GetChildren()
	end))
end
