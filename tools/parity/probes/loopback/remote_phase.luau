-- parity: loopback
-- T2: where in the receiving side's frame what the other side sent shows up: a RemoteEvent's handler, a replicated
-- property's Changed and a replicated child's ChildAdded, a RemoteFunction's callback and the return of the call, each
-- placed by the frame step before it and the one after it: the RunService events, and a legacy wait() loop's resumptions
-- ("wait", every other frame or so). The sends go out from a Heartbeat handler, one a frame, 20 of each. Printed: the set of
-- places each kind arrived at.
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
local COUNT = 20
-- the steps this side takes, by name: each one ends the place an arrival is waiting to be told
local Last, Waiting, Running = "none", {}, true
local function Step(Name)
	for Index, Place in Waiting do
		Place[2] = Name
		Waiting[Index] = nil
	end
	Last = Name
end
local function Track()
	local Names = { "PreAnimation", "PreSimulation", "PostSimulation", "Heartbeat" }
	if RunService:IsClient() then
		table.insert(Names, 1, "PreRender")
	end
	for _, Name in Names do
		RunService[Name]:Connect(function()
			Step(Name)
		end)
	end
	task.spawn(function()
		while Running do
			wait()
			Step("wait")
		end
	end)
end
local Places = {}
local function Arrived(Kind)
	local Place = { Last, "?" }
	table.insert(Waiting, Place)
	Places[Kind] = Places[Kind] or {}
	table.insert(Places[Kind], Place)
end
local function Report(Kind)
	local Seen, List = {}, {}
	for _, Place in Places[Kind] or {} do
		local Text = Place[1] .. " > " .. Place[2]
		if not Seen[Text] then
			Seen[Text] = true
			table.insert(List, Text)
		end
	end
	table.sort(List)
	print(Kind .. " arrived " .. #(Places[Kind] or {}) .. " times, between " .. table.concat(List, " | "))
end
--- server
local Player = P.player()
local Event = Instance.new("RemoteEvent")
Event.Name = "Event"
Event.Parent = Storage
local Call = Instance.new("RemoteFunction")
Call.Name = "Call"
Call.Parent = Storage
local Value = Instance.new("IntValue")
Value.Name = "Value"
Value.Parent = Storage
local Box = Instance.new("Folder")
Box.Name = "Box"
Box.Parent = Storage
Track()
Event.OnServerEvent:Connect(function()
	Arrived("OnServerEvent")
end)
Call.OnServerInvoke = function()
	Arrived("OnServerInvoke")
	return true
end
P.meet("ready")
local Index = 0
local Connection
Connection = RunService.Heartbeat:Connect(function()
	Index += 1
	if Index > COUNT then
		Connection:Disconnect()
		return
	end
	Event:FireClient(Player, Index)
	Value.Value = Index
	local Child = Instance.new("Folder")
	Child.Name = tostring(Index)
	Child.Parent = Box
end)
P.meet("sent")
P.meet("invoked")
for _ = 1, 10 do
	task.wait()
end
Running = false
Report("OnServerEvent")
Report("OnServerInvoke")
--- client
local Event = Storage:WaitForChild("Event")
local Call = Storage:WaitForChild("Call")
local Value = Storage:WaitForChild("Value")
local Box = Storage:WaitForChild("Box")
Track()
Event.OnClientEvent:Connect(function()
	Arrived("OnClientEvent")
end)
Value.Changed:Connect(function()
	Arrived("Changed")
end)
Box.ChildAdded:Connect(function()
	Arrived("ChildAdded")
end)
P.meet("ready")
local Index = 0
local Connection
Connection = RunService.Heartbeat:Connect(function()
	Index += 1
	if Index > COUNT then
		Connection:Disconnect()
		return
	end
	Event:FireServer(Index)
end)
P.meet("sent")
for _ = 1, 10 do
	task.wait()
end
for _ = 1, COUNT do
	RunService.Heartbeat:Wait()
	Call:InvokeServer()
	Arrived("InvokeServer returned")
end
P.meet("invoked")
for _ = 1, 10 do
	task.wait()
end
Running = false
Report("OnClientEvent")
Report("Changed")
Report("ChildAdded")
Report("InvokeServer returned")
