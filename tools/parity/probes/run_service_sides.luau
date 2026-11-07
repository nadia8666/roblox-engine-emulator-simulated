-- What RunService and Players answer on each side: the frame events and render-step bindings a server refuses, where it
-- refuses them (reading the event, or connecting to it), and the Is* answers. Run on a client and on a server (--server).
local RunService = game:GetService("RunService")
local Players = game:GetService("Players")
local function Nothing() end
local Sides = {}
for _, Name in { "IsClient", "IsServer", "IsStudio", "IsRunning", "IsRunMode", "IsEdit" } do
	table.insert(Sides, Name .. "=" .. P.try(function()
		return RunService[Name](RunService)
	end))
end
print("sides " .. table.concat(Sides, " | ") .. " | local-player " .. tostring(Players.LocalPlayer ~= nil))
for _, Name in { "RenderStepped", "PreRender", "PreAnimation", "PreSimulation", "PostSimulation", "Stepped", "Heartbeat" } do
	local Read = P.try(function()
		return typeof(RunService[Name])
	end)
	local Connected = P.try(function()
		local Connection = RunService[Name]:Connect(Nothing)
		local Live = Connection.Connected
		Connection:Disconnect()
		return Live
	end)
	local Once = P.try(function()
		local Connection = RunService[Name]:Once(Nothing)
		Connection:Disconnect()
		return true
	end)
	print("event " .. Name .. " read " .. Read .. " | connect " .. Connected .. " | once " .. Once)
end
print("bind " .. P.try(RunService.BindToRenderStep, RunService, "ParityStep", Enum.RenderPriority.First.Value, Nothing)
	.. " | unbind " .. P.try(RunService.UnbindFromRenderStep, RunService, "ParityStep")
	.. " | unbind-missing " .. P.try(RunService.UnbindFromRenderStep, RunService, "NeverBound"))
print("camera " .. tostring(workspace.CurrentCamera ~= nil) .. " " .. P.try(function()
	return workspace.CurrentCamera and workspace.CurrentCamera.ViewportSize.X > 0
end))
-- whether the render-side handlers a server accepts ever run (five frames)
local Fired = { PreRender = 0, Bound = 0 }
local Render = RunService.PreRender:Connect(function()
	Fired.PreRender += 1
end)
local Bound = pcall(RunService.BindToRenderStep, RunService, "ParityCount", Enum.RenderPriority.First.Value, function()
	Fired.Bound += 1
end)
for _ = 1, 5 do
	task.wait()
end
Render:Disconnect()
pcall(RunService.UnbindFromRenderStep, RunService, "ParityCount")
print("fires prerender " .. tostring(Fired.PreRender > 0) .. " bound " .. tostring(Bound) .. " " .. tostring(Fired.Bound > 0))
