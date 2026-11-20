-- parity: identity=4 context=edit
-- settings() as a plugin's code gets it: the GlobalSettings object, the same every call, its eight settings services, their
-- values, writes and Changed, and its errors. Captured at script identity 4 in a playing client (the command bar's identity,
-- which has the Plugin capability a game script lacks: global_settings.luau); GlobalSettings is one object a process, and a
-- server's identity 4 reads the same but for its live readings. ree runs this probe in its edit run context (a plugin's
-- code). Live readings (counts of instances and players, free memory, the machine's thread pool) print their type only.
local Root = settings()
local function Game(Text)
	-- the DataModel's name in an error: the real engine's place and ree's are named differently
	return (string.gsub(Text, "parented to " .. string.gsub(game.Name, "%p", "%%%0") .. " %.", "parented to <game> ."))
end
local function Show(Value)
	local Kind = typeof(Value)
	if Kind == "number" then
		return Kind .. " " .. P.num(Value)
	elseif Kind == "Instance" then
		return Kind .. " " .. Value.ClassName .. ":" .. Value.Name .. "<" .. (if Value.Parent then Value.Parent.ClassName else "nil") .. ">"
	end
	return Kind .. " " .. tostring(Value)
end
local function Children(Object)
	local Out = {}
	for _, Child in Object:GetChildren() do
		table.insert(Out, Child.ClassName .. ":" .. Child.Name)
	end
	return #Out .. " " .. table.concat(Out, ",")
end

print("root " .. typeof(Root) .. " " .. Root.ClassName .. " | " .. Root.Name .. " | " .. tostring(Root) .. " | " .. Root:GetFullName())
print("same " .. tostring(settings() == Root) .. " " .. tostring(rawequal(settings(), Root)) .. " parent " .. tostring(Root.Parent) .. " archivable " .. tostring(Root.Archivable))
for _, Class in { "Instance", "ServiceProvider", "GenericSettings", "GlobalSettings", "UserSettings", "DataModel" } do
	print("isa " .. Class .. " " .. tostring(Root:IsA(Class)))
end
print("children " .. Children(Root))
for _, Child in Root:GetChildren() do
	print("child " .. Show(Child) .. " full=" .. Child:GetFullName() .. " tostring=" .. tostring(Child) .. " children=" .. #Child:GetChildren() .. " archivable=" .. tostring(Child.Archivable))
end

for _, Name in { "GameSettings", "LuaSettings", "DebugSettings", "PhysicsSettings", "NetworkSettings", "RenderSettings", "TaskScheduler", "Studio", "UserGameSettings", "GlobalSettings", "Workspace", "Lighting", "Part", "NotAService" } do
	print("service " .. Name .. " find " .. P.try(function()
		return Show(Root:FindService(Name))
	end) .. " | get " .. P.try(function()
		return Show(Root:GetService(Name))
	end))
end
print("children-after " .. Children(Root))
for _, Name in { "Rendering", "Game Options", "Lua", "Diagnostics", "Physics", "Network", "Task Scheduler", "Studio", "RenderSettings", "GameSettings", "Name", "NotAMember" } do
	print("index " .. Name .. " " .. P.try(function()
		return Show(Root[Name])
	end))
end

-- every property rbx-dom lists for the settings services: a setting prints its value, a live reading its type
local Readings = {
	["DebugSettings.DataModel"] = true, ["DebugSettings.InstanceCount"] = true, ["DebugSettings.JobCount"] = true,
	["DebugSettings.PlayerCount"] = true, ["DebugSettings.RobloxVersion"] = true, ["NetworkSettings.EmulatedTotalMemoryInMB"] = true,
	["NetworkSettings.FreeMemoryMBytes"] = true, ["TaskScheduler.SchedulerDutyCycle"] = true, ["TaskScheduler.SchedulerRate"] = true,
	["TaskScheduler.ThreadPoolSize"] = true,
}
local Properties = {
	{ "RenderSettings", { "AutoFRMLevel", "EagerBulkExecution", "EditQualityLevel", "Enable VR Mode", "EnableFRM", "ExportMergeByMaterial", "FrameRateManager", "GraphicsMode", "MeshCacheSize", "MeshPartDetailLevel", "QualityLevel", "ReloadAssets", "RenderCSGTrianglesDebug", "ShowBoundingBoxes", "ViewMode" } },
	{ "GameSettings", { "VideoCaptureEnabled" } },
	{ "DebugSettings", { "DataModel", "InstanceCount", "IsScriptStackTracingEnabled", "JobCount", "PlayerCount", "ReportSoundWarnings", "RobloxVersion", "TickCountPreciseOverride" } },
	{ "PhysicsSettings", { "AllowSleep", "AreAnchorsShown", "AreAssembliesShown", "AreAssemblyCentersOfMassShown", "AreAwakePartsHighlighted", "AreBodyTypesShown", "AreCollisionCostsShown", "AreConstraintForcesShownForSelectedOrHoveredInstances", "AreConstraintTorquesShownForSelectedOrHoveredInstances", "AreContactForcesShownForSelectedOrHoveredAssemblies", "AreContactIslandsShown", "AreContactPointsShown", "AreGravityForcesShownForSelectedOrHoveredAssemblies", "AreJointCoordinatesShown", "AreMagnitudesShownForDrawnForcesAndTorques", "AreMechanismsShown", "AreModelCoordsShown", "AreNonAnchorsShown", "AreOwnersShown", "ArePartCoordsShown", "AreRegionsShown", "AreSolverIslandsShown", "AreTerrainReplicationRegionsShown", "AreTimestepsShown", "AreUnalignedPartsShown", "AreWorldCoordsShown", "DisableCSGv2", "DisableCSGv3ForPlugins", "DrawConstraintsNetForce", "DrawContactsNetForce", "DrawTotalNetForce", "EnableForceVisualizationSmoothing", "FluidForceDrawScale", "ForceCSGv2", "ForceDrawScale", "ForceVisualizationSmoothingSteps", "IsInterpolationThrottleShown", "IsReceiveAgeShown", "IsTreeShown", "PhysicsEnvironmentalThrottle", "ShowDecompositionGeometry", "ShowFluidForcesForSelectedOrHoveredMechanisms", "ShowInstanceNamesForDrawnForcesAndTorques", "SolverConvergenceMetricType", "SolverConvergenceVisualizationMode", "ThrottleAdjustTime", "TorqueDrawScale", "UseCSGv2" } },
	{ "NetworkSettings", { "EmulatedTotalMemoryInMB", "FreeMemoryMBytes", "HttpProxyEnabled", "HttpProxyURL", "IncomingReplicationLag", "OpenCertManagerDialog", "PrintJoinSizeBreakdown", "PrintPhysicsErrors", "PrintStreamInstanceQuota", "RandomizeJoinInstanceOrder", "RenderStreamedRegions", "ShowActiveAnimationAsset" } },
	{ "TaskScheduler", { "SchedulerDutyCycle", "SchedulerRate", "ThreadPoolConfig", "ThreadPoolSize" } },
	{ "LuaSettings", { "GcPause", "DefaultWaitTime" } },
}
for _, Entry in Properties do
	local Class, Names = Entry[1], Entry[2]
	local Service = Root:GetService(Class)
	for _, Property in Names do
		local Key = Class .. "." .. Property
		print("prop " .. Key .. " " .. P.try(function()
			local Value = Service[Property]
			return if Readings[Key] then typeof(Value) else Show(Value)
		end))
	end
end
print("enum " .. P.try(function()
	local Quality = Root.Rendering.QualityLevel
	return Quality.Name, Quality.Value, tostring(Quality.EnumType), Quality == Enum.QualityLevel.Automatic
end))

print("method GetMaxQualityLevel " .. P.try(function()
	return Root.Rendering:GetMaxQualityLevel()
end) .. " | dot " .. P.try(function()
	return Root.Rendering.GetMaxQualityLevel(nil)
end))
print("method GetFFlag " .. P.try(function()
	return Root:GetFFlag("NoSuchFlagAnywhere")
end) .. " | " .. P.try(function()
	return Root:GetFFlag()
end) .. " | " .. P.try(function()
	return Root:GetFFlag(12)
end))
print("method GetFVariable " .. P.try(function()
	return Root:GetFVariable("NoSuchFlagAnywhere")
end) .. " | " .. P.try(function()
	return Root:GetFVariable()
end))
print("method dot " .. P.try(function()
	return Root.GetService(nil, "RenderSettings")
end) .. " | " .. P.try(function()
	return Root.FindService(nil, "RenderSettings")
end))

-- writes: an enum setting takes an item, its value or its name; a read-only one and a missing one refuse
local Rendering = Root.Rendering
local Old = Rendering.QualityLevel
print("write QualityLevel " .. P.try(function()
	Rendering.QualityLevel = Enum.QualityLevel.Level05
	return Show(Rendering.QualityLevel)
end) .. " | number " .. P.try(function()
	Rendering.QualityLevel = 3
	return Show(Rendering.QualityLevel)
end) .. " | name " .. P.try(function()
	Rendering.QualityLevel = "Level07"
	return Show(Rendering.QualityLevel)
end) .. " | bad " .. P.try(function()
	Rendering.QualityLevel = "Nope"
end) .. " | wrong enum " .. P.try(function()
	Rendering.QualityLevel = Enum.Material.Wood
end))
Rendering.QualityLevel = Old
print("restored " .. Show(Rendering.QualityLevel))
print("write read-only " .. P.try(function()
	Root.Diagnostics.RobloxVersion = "x"
end) .. " | " .. P.try(function()
	Root["Task Scheduler"].ThreadPoolSize = 4
end) .. " | " .. P.try(function()
	Root.Network.HttpProxyURL = "x"
end) .. " | " .. P.try(function()
	Root.Network.HttpProxyEnabled = true
end))
print("write missing " .. P.try(function()
	Rendering.NotAProp = 1
end) .. " | root " .. P.try(function()
	Root.NotAProp = 1
end))
print("missing " .. P.try(function()
	return Rendering.NotAProp
end) .. " | root " .. P.try(function()
	return Root.NotAProp
end) .. " | method " .. P.try(function()
	return Root:NotAMethod()
end))

-- Changed: a write of each of these settings, then two resumptions
local function Watch(Class, Property, Value)
	local Service = Root:GetService(Class)
	local Seen = {}
	local Changed = Service.Changed:Connect(function(Name)
		table.insert(Seen, Name)
	end)
	local Signal = Service:GetPropertyChangedSignal(Property):Connect(function()
		table.insert(Seen, "signal")
	end)
	local Was = Service[Property]
	local Wrote = P.try(function()
		Service[Property] = Value
		return Show(Service[Property])
	end)
	task.wait()
	task.wait()
	Changed:Disconnect()
	Signal:Disconnect()
	Service[Property] = Was
	task.wait()
	print("changed " .. Class .. "." .. Property .. " " .. Wrote .. " | seen " .. table.concat(Seen, ",") .. " | restored " .. Show(Service[Property]))
end
Watch("RenderSettings", "QualityLevel", Enum.QualityLevel.Level04)
Watch("RenderSettings", "EditQualityLevel", Enum.QualityLevel.Level04)
Watch("RenderSettings", "ShowBoundingBoxes", true)
Watch("RenderSettings", "MeshPartDetailLevel", Enum.MeshPartDetailLevel.Level02)
Watch("RenderSettings", "AutoFRMLevel", 3)
Watch("RenderSettings", "EagerBulkExecution", true)
Watch("RenderSettings", "ExportMergeByMaterial", true)
Watch("RenderSettings", "RenderCSGTrianglesDebug", true)
Watch("RenderSettings", "ViewMode", Enum.ViewMode.Transparent)
Watch("RenderSettings", "EnableFRM", false)
Watch("RenderSettings", "FrameRateManager", Enum.FramerateManagerMode.Off)
Watch("RenderSettings", "MeshCacheSize", 1048576)
Watch("PhysicsSettings", "AllowSleep", false)
Watch("PhysicsSettings", "AreAnchorsShown", true)
Watch("PhysicsSettings", "ThrottleAdjustTime", 2)
Watch("DebugSettings", "ReportSoundWarnings", true)
Watch("DebugSettings", "IsScriptStackTracingEnabled", false)
Watch("GameSettings", "VideoCaptureEnabled", false)
Watch("NetworkSettings", "PrintPhysicsErrors", true)
Watch("NetworkSettings", "RandomizeJoinInstanceOrder", false)

-- the tree: services keep their parents and the root its own, nothing clones, nothing is added, nothing is destroyed
print("parent child " .. P.try(function()
	Rendering.Parent = workspace
	local Moved = Show(Rendering.Parent)
	Rendering.Parent = Root
	return Moved
end) .. " | root " .. P.try(function()
	Root.Parent = workspace
	local Moved = Show(Root.Parent)
	Root.Parent = nil
	return Moved
end))
print("clone child " .. P.try(function()
	return Show(Rendering:Clone())
end) .. " | root " .. P.try(function()
	return Show(Root:Clone())
end))
print("add root " .. P.try(function()
	local Folder = Instance.new("Folder")
	Folder.Parent = Root
	local Where = Show(Folder.Parent)
	Folder:Destroy()
	return Where
end) .. " | child " .. P.try(function()
	local Folder = Instance.new("Folder")
	Folder.Parent = Rendering
	local Where = Show(Folder.Parent)
	Folder:Destroy()
	return Where
end))
print("rename root " .. P.try(function()
	local Was = Root.Name
	Root.Name = "Renamed Settings"
	local Now = Root.Name .. " | " .. tostring(Root) .. " | " .. Root:GetFullName() .. " | " .. Rendering:GetFullName()
	Root.Name = Was
	return Now
end) .. " | child " .. P.try(function()
	local Was = Rendering.Name
	Rendering.Name = "Renamed"
	local Now = Rendering.Name .. " | " .. tostring(Root:FindFirstChild("Renamed"))
	Rendering.Name = Was
	return Now
end))
print("rename services " .. (function()
	local Out = {}
	for _, Child in Root:GetChildren() do
		local Was = Child.Name
		table.insert(Out, Child.ClassName .. "=" .. P.try(function()
			Child.Name = "Renamed"
			return Child.Name
		end))
		pcall(function()
			Child.Name = Was
		end)
	end
	return table.concat(Out, ", ")
end)())
print("generic " .. P.try(function()
	return Show(Root:FindFirstChild("Rendering")), Show(Root:FindFirstChildOfClass("RenderSettings")), Show(Root:WaitForChild("Lua")), Root:IsAncestorOf(Rendering), Rendering:IsDescendantOf(Root)
end))
print("destroy child " .. P.try(function()
	Root.Lua:Destroy()
	return Show(Root:FindFirstChild("Lua"))
end) .. " | root " .. P.try(function()
	Root:Destroy()
	return #Root:GetChildren()
end))
print("children-end " .. Children(Root))

-- the other providers refuse the settings services as their children
for _, Name in { "GameSettings", "LuaSettings", "DebugSettings", "PhysicsSettings", "NetworkSettings", "RenderSettings", "TaskScheduler", "Studio", "UserGameSettings", "GlobalSettings" } do
	print("game " .. Name .. " " .. Game(P.try(function()
		return Show(game:GetService(Name))
	end)) .. " | find " .. P.try(function()
		return Show(game:FindService(Name))
	end) .. " | user " .. P.try(function()
		return Show(UserSettings():GetService(Name))
	end) .. " | find " .. P.try(function()
		return Show(UserSettings():FindService(Name))
	end))
end

-- Studio's settings service is there; ree does not emulate its members
print("studio " .. P.try(function()
	local Studio = Root.Studio
	return typeof(Studio.Theme), typeof(Studio["Tab Width"]), typeof(Studio.ScriptTimeoutLength)
end) .. " | " .. P.try(function()
	return typeof(Root.Studio.ThemeChanged)
end))
