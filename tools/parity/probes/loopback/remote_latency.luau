-- parity: loopback
-- T1: how many of its own frames a side waits for an answer: a RemoteEvent echoed by the other side's handler, from
-- either side, and a RemoteFunction's round trip, each 60 times. A side counts its own frames at PreSimulation (the two
-- sides' frames are never compared; a count at PreSimulation is the same in any Heartbeat handler, whatever order the
-- handlers run in), and the sends go out from a Heartbeat handler, one a frame. The fewest frames are printed: a real
-- session's spread varies with the machine's load (the most frames do too), the fewest is the link's own latency.
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
local COUNT = 60
local function Counter()
	local Count = 0
	RunService.PreSimulation:Connect(function()
		Count += 1
	end)
	return function()
		return Count
	end
end
local function Fewest(List)
	local Low = math.huge
	for _, Value in List do
		Low = math.min(Low, Value)
	end
	return Low
end
--- server
local Player = P.player()
local Frame = Counter()
local Ping = Instance.new("RemoteEvent")
Ping.Name = "Ping"
Ping.Parent = Storage
local Pong = Instance.new("RemoteEvent")
Pong.Name = "Pong"
Pong.Parent = Storage
local Call = Instance.new("RemoteFunction")
Call.Name = "Call"
Call.Parent = Storage
-- the client's pings come back at once
Ping.OnServerEvent:Connect(function(From, Index)
	Ping:FireClient(From, Index)
end)
Call.OnServerInvoke = function(_, Index)
	return Index
end
-- an event and an invoke sent together: the frames between the event's arrival and the invoke's callback here
local Paired = Instance.new("RemoteEvent")
Paired.Name = "Paired"
Paired.Parent = Storage
local PairedCall = Instance.new("RemoteFunction")
PairedCall.Name = "PairedCall"
PairedCall.Parent = Storage
local EventAt, Gaps = {}, {}
Paired.OnServerEvent:Connect(function(From, Index)
	EventAt[Index] = Frame()
	Paired:FireClient(From, Index)
end)
PairedCall.OnServerInvoke = function(_, Index)
	if EventAt[Index] then
		Gaps[Frame() - EventAt[Index]] = true
	end
	return Index
end
local Sent, Back, Arrived = {}, {}, 0
Pong.OnServerEvent:Connect(function(_, Index)
	Back[Index] = Frame() - Sent[Index]
	Arrived += 1
end)
P.meet("ready")
-- the server's pings, one a Heartbeat
local Index = 0
local Connection
Connection = RunService.Heartbeat:Connect(function()
	Index += 1
	if Index > COUNT then
		Connection:Disconnect()
		return
	end
	Sent[Index] = Frame()
	Pong:FireClient(Player, Index)
end)
local Deadline = os.clock() + 30
while Arrived < COUNT and os.clock() < Deadline do
	task.wait()
end
print("server-origin event round trip: " .. Arrived .. " back, fewest frames " .. Fewest(Back))
P.meet("server done")
P.meet("paired done")
local List = {}
for Gap in Gaps do
	table.insert(List, Gap)
end
table.sort(List)
print("invoke callback minus the arrival of the event sent with it, frames seen: " .. table.concat(List, " "))
--- client
local Ping = Storage:WaitForChild("Ping")
local Pong = Storage:WaitForChild("Pong")
local Call = Storage:WaitForChild("Call")
Pong.OnClientEvent:Connect(function(Index)
	Pong:FireServer(Index)
end)
local Frame = Counter()
local Sent, Back, Arrived = {}, {}, 0
Ping.OnClientEvent:Connect(function(Index)
	Back[Index] = Frame() - Sent[Index]
	Arrived += 1
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
	Sent[Index] = Frame()
	Ping:FireServer(Index)
end)
local Deadline = os.clock() + 30
while Arrived < COUNT and os.clock() < Deadline do
	task.wait()
end
print("client-origin event round trip: " .. Arrived .. " back, fewest frames " .. Fewest(Back))
P.meet("server done")
-- invokes, one after another, each from a Heartbeat handler's thread
local Frames = {}
for Call_ = 1, COUNT do
	RunService.Heartbeat:Wait()
	local Start = Frame()
	local Answer = Call:InvokeServer(Call_)
	if Answer == Call_ then
		table.insert(Frames, Frame() - Start)
	end
end
print("InvokeServer round trip: " .. #Frames .. " answered, fewest frames " .. Fewest(Frames))
-- an event and an invoke sent together from one Heartbeat: the frames between the event's echo and the invoke's return
local Paired = Storage:WaitForChild("Paired")
local PairedCall = Storage:WaitForChild("PairedCall")
local EchoAt, ReturnAt, Gaps = {}, {}, {}
Paired.OnClientEvent:Connect(function(Index)
	EchoAt[Index] = Frame()
end)
for Pair = 1, COUNT do
	RunService.Heartbeat:Wait()
	Paired:FireServer(Pair)
	PairedCall:InvokeServer(Pair)
	ReturnAt[Pair] = Frame()
end
for _ = 1, 10 do
	task.wait()
end
for Pair, At in ReturnAt do
	if EchoAt[Pair] then
		Gaps[At - EchoAt[Pair]] = true
	end
end
local List = {}
for Gap in Gaps do
	table.insert(List, Gap)
end
table.sort(List)
P.meet("paired done")
print("invoke return minus the echo of the event sent with it, frames seen: " .. table.concat(List, " "))
