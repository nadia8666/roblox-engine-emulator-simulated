-- parity: loopback
-- T3: what a table sent through a RemoteEvent arrives as, each way: arrays with holes, mixed and non-string keys, nested,
-- shared and cyclic subtables, metatables, frozen tables, functions and instances inside, how deep a table may nest, whether
-- each handler gets a table of its own, and whether a change the sender makes after the call arrives.
local Storage = game:GetService("ReplicatedStorage")
local function Key(Value)
	local Kind = typeof(Value)
	if Kind == "number" then
		return "n" .. P.num(Value)
	elseif Kind == "string" then
		-- (a table key arrives as a text holding its address)
		return "s" .. P.text((string.gsub(Value, "0x%x+", "ADDR")))
	elseif Kind == "Instance" then
		return "I" .. Value.Name
	end
	return Kind
end
-- a table as text: its entries sorted by key, nested tables inside braces, marks for a metatable and a frozen table
local function Dump(Value, Depth)
	Depth = Depth or 0
	local Kind = typeof(Value)
	if Kind ~= "table" then
		if Kind == "number" then
			return P.num(Value)
		elseif Kind == "string" then
			return "'" .. P.text(Value) .. "'"
		elseif Kind == "Instance" then
			return "<" .. Value.Name .. ">"
		elseif Kind == "Vector3" then
			return "V(" .. P.f32s(Value.X, Value.Y, Value.Z) .. ")"
		end
		return Kind == "nil" and "nil" or Kind == "boolean" and tostring(Value) or Kind
	end
	if Depth > 6 then
		return "{...}"
	end
	local Keys = {}
	for K in Value do
		table.insert(Keys, K)
	end
	table.sort(Keys, function(A, B)
		return Key(A) < Key(B)
	end)
	local Parts = {}
	for _, K in Keys do
		table.insert(Parts, "[" .. Key(K) .. "]=" .. Dump(Value[K], Depth + 1))
	end
	local Marks = (getmetatable(Value) ~= nil and "mt " or "") .. (table.isfrozen(Value) and "frozen " or "")
	return Marks .. "{" .. table.concat(Parts, ",") .. "}"
end
local function Nest(Depth)
	local Root = {}
	local At = Root
	for _ = 2, Depth do
		local Inner = {}
		At[1] = Inner
		At = Inner
	end
	return Root
end
local function Height(Value)
	local Count = 0
	while type(Value) == "table" do
		Count += 1
		Value = Value[1]
	end
	return Count
end
local function Cases(Own)
	local Shared = { 9 }
	local Cycle = { 1 }
	Cycle.Self = Cycle
	local Inner = { 1 }
	local Deep = { Inner }
	Inner[2] = Deep
	return {
		{ "empty", {} },
		{ "array", { 1, 2, 3 } },
		{ "hole-middle", { [1] = 1, [3] = 3 } },
		{ "hole-start", { [2] = 2, [3] = 3 } },
		{ "hole-end", { 1, 2, nil } },
		{ "sparse-far", { [1] = 1, [100] = 100 } },
		{ "zero-negative", { [0] = 0, [-1] = -1, [1] = 1 } },
		{ "fraction-key", { [1.5] = 1, [1] = 2 } },
		{ "big-key", { [2 ^ 31] = 1, [2 ^ 53] = 2 } },
		{ "dictionary", { a = 1, b = "x" } },
		{ "mixed", { 1, 2, a = 3 } },
		{ "mixed-hole", { [1] = 1, [3] = 3, a = 4 } },
		{ "bool-key", { [true] = 1, [false] = 2 } },
		{ "vector-key", { [Vector3.new(1, 2, 3)] = 1 } },
		{ "instance-key", { [Own.Shared] = 1 } },
		{ "table-key", { [{}] = 1 } },
		{ "collide", { [1] = "number", ["1"] = "string" } },
		{ "nul-key", { ["a\0b"] = 1 } },
		{ "nested", { a = { b = { c = 1 } } } },
		{ "shared", { a = Shared, b = Shared } },
		{ "cycle", Cycle },
		{ "deep-cycle", Deep },
		{ "metatable", setmetatable({ x = 1 }, { __index = function() return 5 end }) },
		{ "frozen", table.freeze({ x = 1 }) },
		{ "function-value", { f = print, g = 1 } },
		{ "function-array", { 1, print, 3 } },
		{ "thread-value", { t = coroutine.create(print), g = 1 } },
		{ "instance-array", { Own.Shared, Own.Mine, Own.Shared } },
		{ "instance-value", { a = Own.Shared, b = Own.Mine } },
		{ "vectors", { Vector3.new(1, 2, 3), vector.create(4, 5, 6) } },
		{ "nest-20", Nest(20) },
		{ "nest-100", Nest(100) },
		{ "nest-1000", Nest(1000) },
	}
end
local function Report(Index, Name, Value, Extra)
	local Text = if Name:sub(1, 5) == "nest-" then "height " .. Height(Value) else Dump(Value)
	return "case " .. Index .. " " .. Name .. " " .. Text .. (Extra or "")
end
local function Send(Fire, Own)
	for Index, Case in Cases(Own) do
		local Ok, Problem = pcall(Fire, Index, Case[1], Case[2])
		if not Ok then
			print("send " .. Index .. " " .. Case[1] .. " error " .. P.err(Problem))
		end
	end
	-- the sender changes a table after the call: the receiver gets it as it was at the call
	local Changed = { v = 1 }
	Fire(0, "changed-after", Changed)
	Changed.v = 2
	Changed.w = 3
end
-- two handlers of one remote: whether each gets a table of its own
local Seen = {}
local function Second(Index, _, Value)
	if Index == 2 then
		Seen.Second = Value
	end
end
local function Identity()
	return "handlers share the table: " .. tostring(Seen.First ~= nil and rawequal(Seen.First, Seen.Second))
end
--- server
local Player = P.player()
local Event = Instance.new("RemoteEvent")
Event.Name = "Tables"
Event.Parent = Storage
local Shared = Instance.new("Part")
Shared.Name = "Shared"
Shared.Parent = workspace
local Mine = Instance.new("Folder")
Mine.Name = "ServerOnly"
Mine.Parent = game:GetService("ServerStorage")
local Got = {}
Event.OnServerEvent:Connect(function(_, Index, Name, Value)
	if Index == 2 then
		Seen.First = Value
	end
	table.insert(Got, Report(Index, Name, Value))
end)
Event.OnServerEvent:Connect(function(_, ...)
	Second(...)
end)
P.meet("ready")
Send(function(...)
	Event:FireClient(Player, ...)
end, { Shared = Shared, Mine = Mine })
P.meet("sent")
P.meet("client sent")
for _ = 1, 10 do
	task.wait()
end
for _, Line in Got do
	print(Line)
end
print(Identity())
--- client
local Event = Storage:WaitForChild("Tables")
local Shared = workspace:WaitForChild("Shared")
local Got = {}
Event.OnClientEvent:Connect(function(Index, Name, Value)
	if Index == 2 then
		Seen.First = Value
	end
	table.insert(Got, Report(Index, Name, Value))
end)
Event.OnClientEvent:Connect(Second)
P.meet("ready")
P.meet("sent")
for _ = 1, 10 do
	task.wait()
end
for _, Line in Got do
	print(Line)
end
print(Identity())
Seen = {}
local Mine = Instance.new("Folder")
Mine.Name = "ClientOnly"
Mine.Parent = Storage
Send(function(...)
	Event:FireServer(...)
end, { Shared = Shared, Mine = Mine })
P.meet("client sent")
