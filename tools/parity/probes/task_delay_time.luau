-- When a task.delay / task.wait thread is due: its delay counts from the moment of the call (the clock os.clock reads,
-- not the start of the frame), and it resumes at the first resumption phase whose time is at least that far on. So a
-- delay of whole frames started late in a frame (a RenderStepped handler) takes one frame more; one 3 ms short of whole
-- frames started in a Heartbeat handler does not. (Delays that land on a frame boundary from a Heartbeat handler or a
-- resumed thread fall either way in a real client, by its frame times: not printed.)
local RunService = game:GetService("RunService")
P.sync()
local Frames = 0
local Counter = RunService.Heartbeat:Connect(function()
	Frames += 1
end)
local Results = {}
local function Start(Label, Delay)
	local From = Frames
	task.delay(Delay, function()
		table.insert(Results, Label .. "=" .. (Frames - From))
	end)
	task.spawn(function()
		local Begin = Frames
		local Elapsed = task.wait(Delay)
		table.insert(Results, Label .. "-wait=" .. (Frames - Begin) .. (if Elapsed >= Delay then "" else " short"))
	end)
end
local Started = RunService:IsServer() -- a server has no render step
local Render = if Started then nil else RunService.RenderStepped:Connect(function()
	if Started then
		return
	end
	Started = true
	for Whole = 1, 3 do
		Start("render-" .. Whole, Whole / 60)
	end
end)
task.wait()
task.wait()
local Beat
Beat = RunService.Heartbeat:Connect(function()
	Beat:Disconnect()
	for Whole = 1, 3 do
		Start("heartbeat-short-" .. Whole, Whole / 60 - 0.003)
		Start("heartbeat-long-" .. Whole, Whole / 60 + 0.003)
	end
end)
for _ = 1, 12 do
	task.wait()
end
if Render then
	Render:Disconnect()
end
Counter:Disconnect()
table.sort(Results)
print("delay-frames " .. table.concat(Results, " "))
