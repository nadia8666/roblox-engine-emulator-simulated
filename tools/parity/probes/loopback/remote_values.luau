-- parity: loopback
-- T3: what a RemoteEvent's arguments arrive as, each way: nil, booleans, numbers (NaN, infinities, -0), strings (NUL, invalid
-- UTF-8), buffers, functions, threads, datatypes and EnumItems, and instances the receiver has, has not, or no longer has.
-- Each case is one FireClient / FireServer, all sent in one frame; the receiver prints the count it got and each value.
local Storage = game:GetService("ReplicatedStorage")
local function Hex(Text)
	return (string.gsub(Text, ".", function(Byte)
		return string.format("%02x", string.byte(Byte))
	end))
end
local function Dump(Value, Expected)
	local Kind = typeof(Value)
	if Kind == "nil" or Kind == "boolean" then
		return tostring(Value)
	elseif Kind == "number" then
		return "number " .. P.num(Value)
	elseif Kind == "string" then
		return "string " .. #Value .. " " .. Hex(string.sub(Value, 1, 12))
	elseif Kind == "buffer" then
		return "buffer " .. buffer.len(Value) .. " " .. Hex(buffer.readstring(Value, 0, math.min(12, buffer.len(Value))))
	elseif Kind == "Instance" then
		return "Instance " .. Value.Name .. " in " .. tostring(Value.Parent)
	elseif Kind == "Vector3" then
		return "Vector3 " .. P.f32s(Value.X, Value.Y, Value.Z)
	elseif Kind == "Vector2" then
		return "Vector2 " .. P.f32s(Value.X, Value.Y)
	elseif Kind == "CFrame" then
		return "CFrame " .. P.f32s(Value:GetComponents())
	elseif Kind == "Color3" then
		return "Color3 " .. P.f32s(Value.R, Value.G, Value.B)
	elseif Kind == "UDim2" then
		return "UDim2 " .. P.f32s(Value.X.Scale, Value.X.Offset, Value.Y.Scale, Value.Y.Offset)
	elseif Kind == "table" then
		local Count = 0
		for _ in Value do
			Count += 1
		end
		return "table " .. Count
	elseif Kind == "EnumItem" or Kind == "Enum" then
		return Kind .. " " .. tostring(Value)
	end
	-- other datatypes: whether the value equals the one the case sent (their text is the datatypes probes' to check)
	if Expected ~= nil then
		return Kind .. (if Value == Expected then " equal" else " differs")
	end
	return Kind
end
-- the cases, each a function giving the arguments (the instance cases are given the side's own instances)
local function Cases(Own)
	return {
		{ "none", function() return end },
		{ "nil", function() return nil end },
		{ "nil-nil-1", function() return nil, nil, 1 end },
		{ "1-nil-nil", function() return 1, nil, nil end },
		{ "booleans", function() return true, false end },
		{ "numbers", function() return 0, -0, 1 / 3, 2 ^ 53 + 2, 1e308, 5e-324 end },
		{ "nan-inf", function() return 0 / 0, math.huge, -math.huge end },
		{ "strings", function() return "", "a", string.rep("x", 1000) end },
		{ "nul", function() return "a\0b", "\0" end },
		{ "invalid-utf8", function() return "\255\254", "\237\160\128", "a\128" end },
		{ "buffer", function() local B = buffer.create(5) buffer.writeu8(B, 0, 7) buffer.writeu8(B, 4, 255) return B end },
		{ "empty-buffer", function() return buffer.create(0) end },
		{ "function", function() return print, 1 end },
		{ "thread", function() return coroutine.create(print), 2 end },
		{ "userdata", function() return newproxy(true), 3 end },
		{ "Vector3", function() return Vector3.new(1, 2.5, -3), vector.create(0.1, 0, 0) end },
		{ "Vector2", function() return Vector2.new(4, 5) end },
		{ "CFrame", function() return CFrame.new(1, 2, 3) * CFrame.Angles(0.5, 0, 0) end },
		{ "Color3", function() return Color3.new(0.25, 0.5, 1) end },
		{ "UDim2", function() return UDim2.new(0.5, 10, 1, -4) end },
		{ "UDim", function() return UDim.new(0.25, 3) end },
		{ "BrickColor", function() return BrickColor.new("Bright red") end },
		{ "NumberRange", function() return NumberRange.new(1, 2) end },
		{ "NumberSequence", function() return NumberSequence.new(0, 1) end },
		{ "ColorSequence", function() return ColorSequence.new(Color3.new(1, 0, 0)) end },
		{ "Rect", function() return Rect.new(1, 2, 3, 4) end },
		{ "Ray", function() return Ray.new(Vector3.zero, Vector3.xAxis) end },
		{ "EnumItem", function() return Enum.Material.Wood, Enum.KeyCode.E end },
		{ "Enum", function() return Enum.Material end },
		{ "Font", function() return Font.new("rbxasset://fonts/families/GothamSSm.json", Enum.FontWeight.Bold) end },
		{ "DateTime", function() return DateTime.fromUnixTimestamp(1000) end },
		{ "PhysicalProperties", function() return PhysicalProperties.new(1, 0.5, 0.25) end },
		{ "Random", function() return Random.new(1), 4 end },
		{ "TweenInfo", function() return TweenInfo.new(2), 5 end },
		{ "RaycastParams", function() return RaycastParams.new(), 6 end },
		{ "Instance-shared", function() return Own.Shared end },
		{ "Instance-own", function() return Own.Mine end },
		{ "Instance-parentless", function() return Own.Loose end },
		{ "Instance-destroyed", function() return Own.Gone end },
		{ "Instance-new", function() return Own.Fresh end },
		{ "player", function() return Own.Player end },
		{ "many", function() return table.unpack(table.create(40, 7)) end },
	}
end
local function Receive(Index, ...)
	local Parts = { "n=" .. select("#", ...) }
	-- (the values the case sends, made here again: a datatype is compared with its own)
	local Sent = table.pack(Cases({})[Index][2]())
	for Position = 1, select("#", ...) do
		table.insert(Parts, Dump((select(Position, ...)), Sent[Position]))
	end
	return "case " .. Index .. " " .. table.concat(Parts, " | ")
end
local function Send(Fire, Own)
	for Index, Case in Cases(Own) do
		local Ok, Problem = pcall(Fire, Index, Case[2]())
		if not Ok then
			print("send " .. Index .. " " .. Case[1] .. " error " .. P.err(Problem))
		end
	end
end
--- server
local Player = P.player()
local Event = Instance.new("RemoteEvent")
Event.Name = "Values"
Event.Parent = Storage
local Shared = Instance.new("Part")
Shared.Name = "Shared"
Shared.Parent = workspace
local Mine = Instance.new("Folder")
Mine.Name = "ServerOnly"
Mine.Parent = game:GetService("ServerStorage")
local Gone = Instance.new("Folder")
Gone.Name = "Gone"
Gone.Parent = Storage
local Got = {}
Event.OnServerEvent:Connect(function(From, Index, ...)
	table.insert(Got, (From == Player and "from-player " or "from-other ") .. Receive(Index, ...))
end)
P.meet("ready")
Gone:Destroy()
local Fresh = Instance.new("Folder")
Fresh.Name = "Fresh"
Fresh.Parent = Storage
Send(function(...)
	Event:FireClient(Player, ...)
end, { Shared = Shared, Mine = Mine, Loose = Instance.new("Folder"), Gone = Gone, Fresh = Fresh, Player = Player })
P.meet("sent")
P.meet("client sent")
for _ = 1, 10 do
	task.wait()
end
for _, Line in Got do
	print(Line)
end
--- client
local Event = Storage:WaitForChild("Values")
local Shared = workspace:WaitForChild("Shared")
local Gone = Storage:WaitForChild("Gone")
local Got = {}
Event.OnClientEvent:Connect(function(Index, ...)
	table.insert(Got, Receive(Index, ...))
end)
P.meet("ready")
P.meet("sent")
for _ = 1, 10 do
	task.wait()
end
for _, Line in Got do
	print(Line)
end
local Mine = Instance.new("Folder")
Mine.Name = "ClientOnly"
Mine.Parent = Storage
local Fresh = Instance.new("Folder")
Fresh.Name = "ClientFresh"
Fresh.Parent = workspace
Send(function(...)
	Event:FireServer(...)
end, { Shared = Shared, Mine = Mine, Loose = Instance.new("Folder"), Gone = Gone, Fresh = Fresh, Player = P.player() })
P.meet("client sent")
