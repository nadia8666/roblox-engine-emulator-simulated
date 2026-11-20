-- What a game script gets of the services Roblox keeps for its own scripts, Studio and plugins: GetService's answer,
-- then a property, the class name and a child lookup on what it answered.
local function Kind(Value)
	return typeof(Value)
end
for _, Name in {
	"CoreScriptSyncService", "CoreGui", "CorePackages", "RobloxReplicatedStorage", "StudioService", "ScriptEditorService",
	"PluginGuiService", "RobloxPluginGuiService", "NetworkClient", "AppStorageService", "CSGDictionaryService",
	"ChangeHistoryService", "Selection", "TestService", "BrowserService", "HttpRbxApiService", "ScriptContext", "Visit",
	"VirtualInputManager", "MemStorageService", "ServerScriptService", "ServerStorage", "NotAService",
} do
	local Ok, Service = pcall(game.GetService, game, Name)
	if not Ok then
		print(Name .. " get-error " .. P.err(Service))
		print(Name .. " find " .. P.try(function()
			return Kind(game:FindService(Name))
		end))
		continue
	end
	print(Name .. " get " .. Kind(Service) .. " | name " .. P.try(function()
		return Service.Name
	end) .. " | class " .. P.try(function()
		return Service.ClassName
	end) .. " | tostring " .. P.try(tostring, Service) .. " | parent " .. P.try(function()
		return Service.Parent == game
	end) .. " | child " .. P.try(function()
		return Kind(Service:FindFirstChild("NoSuchChild"))
	end) .. " | isa " .. P.try(function()
		return Service:IsA("Instance")
	end) .. " | missing " .. P.try(function()
		return Service.NoSuchMember
	end))
end
print("sync-path " .. P.try(function()
	return game:GetService("CoreScriptSyncService"):GetScriptFilePath(Instance.new("ModuleScript"))
end))
print("jest-probe " .. P.try(function()
	local Service = game:GetService("CoreScriptSyncService")
	local _ = Service.Name
	return Kind(Service)
end))
-- debug.loadmodule as a game script meets it (tools/parity/probes/loadmodule.luau has the rest, at identity 4)
print("loadmodule " .. type(debug.loadmodule) .. " " .. P.try(function()
	local Results = table.pack(debug.loadmodule(Instance.new("ModuleScript")))
	return type(Results[1]), tostring(Results[2]), type(Results[3]), Results.n
end))
