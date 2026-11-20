-- parity: loopback
-- T4: how much one remote call may carry, each way. An UnreliableRemoteEvent (documented: a payload of at most 1000 bytes)
-- swept across the cutoff with repeated and random bytes as a string and as a buffer, a table of numbers and a table of
-- instance references, against a reliable RemoteEvent sent alongside as a control; then reliable events of 64 KB to 4 MB,
-- a RemoteFunction returning 1 MB, and calls of many arguments. Printed: the largest size that arrived and the smallest
-- that did not, per kind, what the sender's call did, and the warnings each side logged.
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
local LogService = game:GetService("LogService")
local Logged = {}
LogService.MessageOut:Connect(function(Message, Kind)
	if Kind ~= Enum.MessageType.MessageOutput and not string.find(Message, "__Loopback", 1, true) then
		table.insert(Logged, Kind.Name .. " " .. (string.gsub(string.gsub(Message, "\n.*", ""), "%d+", "N")))
	end
end)
local function Logs()
	local Seen, List = {}, {}
	for _, Line in Logged do
		if not Seen[Line] then
			Seen[Line] = true
			table.insert(List, Line)
		end
	end
	table.clear(Logged)
	table.sort(List)
	return table.concat(List, " / ")
end
local function Random(Size, Seed)
	local Rng = P.rng(Seed)
	local Bytes = table.create(Size)
	for Index = 1, Size do
		Bytes[Index] = string.char(Rng.u32() % 256)
	end
	return table.concat(Bytes)
end
-- the payload of a kind at a size (sizes are the kind's own unit: bytes, numbers or references)
local function Payload(Kind, Size, Own)
	if Kind == "string repeated" then
		return string.rep("x", Size)
	elseif Kind == "string random" then
		return Random(Size, Size)
	elseif Kind == "buffer repeated" then
		return buffer.fromstring(string.rep("x", Size))
	elseif Kind == "buffer random" then
		return buffer.fromstring(Random(Size, Size + 7))
	elseif Kind == "numbers" then
		local List = table.create(Size)
		for Index = 1, Size do
			List[Index] = Index + 0.5
		end
		return List
	elseif Kind == "references" then
		return table.create(Size, Own)
	end
end
local SWEEPS = {
	{ "string repeated", 940, 1010, 1 },
	{ "string random", 940, 1010, 1 },
	{ "buffer repeated", 940, 1010, 1 },
	{ "buffer random", 940, 1010, 1 },
	{ "numbers", 80, 140, 1 },
	{ "references", 100, 400, 4 },
}
local Arrived = {}
local function Note(Kind, Size)
	Arrived[Kind] = Arrived[Kind] or {}
	Arrived[Kind][Size] = true
end
-- sends every sweep's sizes, eight a frame, each with a reliable control naming it
local function Sweep(Fire, Own)
	local Errors = {}
	for _, Sweep_ in SWEEPS do
		local Kind, Low, High, Step = table.unpack(Sweep_)
		local Sent = 0
		for Size = Low, High, Step do
			local Ok, Problem = pcall(Fire, Kind, Size, Payload(Kind, Size, Own))
			if not Ok then
				Errors[Kind .. " " .. P.err(Problem)] = true
			end
			Sent += 1
			if Sent % 8 == 0 then
				RunService.Heartbeat:Wait()
			end
		end
	end
	local List = {}
	for Text in Errors do
		table.insert(List, Text)
	end
	table.sort(List)
	return table.concat(List, " / ")
end
local function Report(Control)
	for _, Sweep_ in SWEEPS do
		local Kind, Low, High, Step = table.unpack(Sweep_)
		local Largest, Smallest, Controls = "none", "none", 0
		for Size = Low, High, Step do
			if Control[Kind] and Control[Kind][Size] then
				Controls += 1
			end
			if Arrived[Kind] and Arrived[Kind][Size] then
				Largest = Size
			elseif Smallest == "none" then
				Smallest = Size
			end
		end
		print("unreliable " .. Kind .. ": largest arrived " .. Largest .. ", smallest dropped " .. Smallest .. ", controls "
			.. Controls .. " of " .. (High - Low) // Step + 1)
	end
end
local BIG = { 64 * 1024, 256 * 1024, 1024 * 1024, 4 * 1024 * 1024 }
--- server
local Player = P.player()
local Own = Instance.new("Part")
Own.Name = "Shared"
Own.Parent = workspace
local Unreliable = Instance.new("UnreliableRemoteEvent")
Unreliable.Name = "Unreliable"
Unreliable.Parent = Storage
local Control = Instance.new("RemoteEvent")
Control.Name = "Control"
Control.Parent = Storage
local Big = Instance.new("RemoteEvent")
Big.Name = "Big"
Big.Parent = Storage
local Call = Instance.new("RemoteFunction")
Call.Name = "Call"
Call.Parent = Storage
Call.OnServerInvoke = function(_, Size)
	return string.rep("y", Size)
end
local Controls, Bigs = {}, {}
Unreliable.OnServerEvent:Connect(function(_, Kind, Size)
	Note(Kind, Size)
end)
Control.OnServerEvent:Connect(function(_, Kind, Size)
	Controls[Kind] = Controls[Kind] or {}
	Controls[Kind][Size] = true
end)
Big.OnServerEvent:Connect(function(_, Size, Text, ...)
	table.insert(Bigs, Size .. " " .. tostring(typeof(Text) == "string" and #Text == Size) .. " args " .. select("#", ...))
end)
P.meet("ready")
print("server sends: " .. Sweep(function(Kind, Size, Value)
	Unreliable:FireClient(Player, Kind, Size, Value)
	Control:FireClient(Player, Kind, Size)
end, Own))
for _, Size in BIG do
	print("big " .. Size .. " send " .. P.try(Big.FireClient, Big, Player, Size, string.rep("z", Size)))
end
for _, Count in { 250, 1000, 7000 } do
	print("arguments " .. Count .. " send " .. P.try(Big.FireClient, Big, Player, 0, "", table.unpack(table.create(Count, 1))))
end
P.meet("server sent")
P.meet("client sent")
for _ = 1, 60 do
	task.wait()
end
Report(Controls)
print("big arrived: " .. table.concat(Bigs, " | "))
print("server logs: " .. Logs())
--- client
local Own = workspace:WaitForChild("Shared")
local Unreliable = Storage:WaitForChild("Unreliable")
local Control = Storage:WaitForChild("Control")
local Big = Storage:WaitForChild("Big")
local Call = Storage:WaitForChild("Call")
local Controls, Bigs = {}, {}
Unreliable.OnClientEvent:Connect(function(Kind, Size)
	Note(Kind, Size)
end)
Control.OnClientEvent:Connect(function(Kind, Size)
	Controls[Kind] = Controls[Kind] or {}
	Controls[Kind][Size] = true
end)
Big.OnClientEvent:Connect(function(Size, Text, ...)
	table.insert(Bigs, Size .. " " .. tostring(typeof(Text) == "string" and #Text == Size) .. " args " .. select("#", ...))
end)
P.meet("ready")
P.meet("server sent")
for _ = 1, 60 do
	task.wait()
end
Report(Controls)
print("big arrived: " .. table.concat(Bigs, " | "))
print("client logs: " .. Logs())
table.clear(Arrived)
print("client sends: " .. Sweep(function(Kind, Size, Value)
	Unreliable:FireServer(Kind, Size, Value)
	Control:FireServer(Kind, Size)
end, Own))
for _, Size in BIG do
	print("big " .. Size .. " send " .. P.try(Big.FireServer, Big, Size, string.rep("z", Size)))
end
for _, Count in { 250, 1000, 7000 } do
	print("arguments " .. Count .. " send " .. P.try(Big.FireServer, Big, 0, "", table.unpack(table.create(Count, 1))))
end
local Result = P.try(function()
	return #Call:InvokeServer(1024 * 1024)
end)
print("invoke returning 1 MB " .. Result)
P.meet("client sent")
print("client logs after sending: " .. Logs())
