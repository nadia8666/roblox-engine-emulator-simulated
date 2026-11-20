-- settings() and the settings services as a game script gets them (script identity 2): settings() is refused whatever it
-- is called with, the DataModel's and UserSettings()' GetService answer a settings service's class with nil or refuse it
-- as their child, and UserSettings() makes no other service under it. global_settings_tree.luau has what a plugin gets.
local function Game(Text)
	-- the DataModel's name in an error: the real engine's place and ree's are named differently
	return (string.gsub(Text, "parented to " .. string.gsub(game.Name, "%p", "%%%0") .. " %.", "parented to <game> ."))
end
local function Short(Value)
	if typeof(Value) == "Instance" then
		return Value.ClassName .. ":" .. Value.Name .. "<" .. (if Value.Parent then Value.Parent.ClassName else "nil") .. ">"
	end
	return tostring(Value)
end
print("global " .. type(settings) .. " " .. tostring(settings == settings))
print("call " .. P.try(settings) .. " | " .. P.try(settings, 1, "x") .. " | " .. P.try(function()
	return settings().Rendering.QualityLevel
end))
print("caught " .. (function()
	local Ok, Problem = pcall(settings)
	return tostring(Ok) .. " " .. typeof(Problem) .. " position=" .. tostring(string.find(tostring(Problem), ":%d+:") ~= nil)
end)())
local Classes = { "GameSettings", "LuaSettings", "DebugSettings", "PhysicsSettings", "NetworkSettings", "RenderSettings", "TaskScheduler", "Studio", "UserGameSettings", "GlobalSettings", "UserSettings", "GenericSettings", "ServiceProvider" }
for _, Name in Classes do
	print("game " .. Name .. " " .. Game(P.try(function()
		return Short(game:GetService(Name))
	end)) .. " | find " .. P.try(function()
		return Short(game:FindService(Name))
	end))
end
for _, Name in Classes do
	print("user " .. Name .. " " .. P.try(function()
		return Short(UserSettings():GetService(Name))
	end) .. " | find " .. P.try(function()
		return Short(UserSettings():FindService(Name))
	end))
end
-- other services: UserSettings() makes none of them; some it refuses, as settings() does, but for the ones a plugin has
for _, Name in { "Players", "Workspace", "RunService", "TweenService", "CoreGui", "ReplicatedStorage", "Lighting", "HttpService", "Teams", "DraftsService", "DraggerService", "NetworkClient", "ScriptEditorService", "SerializationService", "StudioService", "TeamCreateData", "TeamCreateService", "VersionControlService", "NetworkServer", "Part", "NotAService" } do
	print("user-other " .. Name .. " " .. P.try(function()
		return Short(UserSettings():GetService(Name))
	end) .. " | find " .. P.try(function()
		return Short(UserSettings():FindService(Name))
	end))
end
local Root = UserSettings()
local GameSettings = Root:GetService("UserGameSettings")
print("user-children " .. (function()
	local Out = {}
	for _, Child in Root:GetChildren() do
		table.insert(Out, Short(Child))
	end
	return table.concat(Out, ",")
end)())
-- the tree's own rules: the root and its service keep their parents, neither clones, nothing goes under them
print("user-parent " .. P.try(function()
	Root.Parent = workspace
	local Moved = Short(Root.Parent)
	Root.Parent = nil
	return Moved
end) .. " | " .. P.try(function()
	GameSettings.Parent = workspace
	local Moved = Short(GameSettings.Parent)
	GameSettings.Parent = Root
	return Moved
end))
print("user-rename " .. P.try(function()
	Root.Name = "Renamed Settings"
	local Now = Root.Name .. " " .. tostring(Root)
	Root.Name = "GlobalBasicSettings"
	return Now
end) .. " | " .. P.try(function()
	GameSettings.Name = "Renamed"
	local Now = GameSettings.Name
	GameSettings.Name = "GameSettings"
	return Now
end))
print("user-clone " .. P.try(function()
	return Short(Root:Clone())
end) .. " | " .. P.try(function()
	return Short(GameSettings:Clone())
end))
print("user-add " .. P.try(function()
	local Folder = Instance.new("Folder")
	Folder.Parent = Root
	local Where = Short(Folder.Parent)
	Folder:Destroy()
	return Where
end) .. " | " .. P.try(function()
	local Folder = Instance.new("Folder")
	Folder.Parent = GameSettings
	local Where = Short(Folder.Parent)
	Folder:Destroy()
	return Where
end))
