-- parity: loopback
-- T14: RemoteFunction both ways: several returns, nil returns, a returned table and instances, a callback that yields, one
-- that errors (the text the caller gets), one that errors after yielding, a callback replaced while calls wait, concurrent
-- calls that finish out of order, and calls nested inside a callback.
local Storage = game:GetService("ReplicatedStorage")
local function Pack(...)
	local Parts = { "n=" .. select("#", ...) }
	for Index = 1, select("#", ...) do
		local Value = select(Index, ...)
		if typeof(Value) == "table" then
			local Keys = {}
			for Key, Item in Value do
				table.insert(Keys, tostring(Key) .. "=" .. tostring(Item))
			end
			table.sort(Keys)
			table.insert(Parts, "{" .. table.concat(Keys, ",") .. "}")
		elseif typeof(Value) == "Instance" then
			table.insert(Parts, "<" .. Value.Name .. ">")
		else
			table.insert(Parts, tostring(Value))
		end
	end
	return table.concat(Parts, " ")
end
local function Call(Invoke, ...)
	local Results = table.pack(pcall(Invoke, ...))
	if Results[1] then
		return "ok " .. Pack(table.unpack(Results, 2, Results.n))
	end
	-- (an error raised in the other side's callback carries that side's script and line: whether it has one)
	local Message = Results[2]
	return "error " .. typeof(Message) .. " " .. (string.gsub(tostring(Message), "^[^\n]-:%d+: ", "<position>: "))
end
-- the callback for each case, by name: what it gets after the player (server) or alone (client)
local function Callbacks(Own)
	return {
		several = function() return 1, nil, "three", nil end,
		none = function() end,
		table = function() return { 1, 2, a = 3 }, { x = Own.Shared, y = Own.Mine } end,
		yields = function(Value) task.wait(0.1) return Value end,
		errors = function() error("callback failed") end,
		["errors-plain"] = function() error("plain failure", 0) end,
		["errors-table"] = function() error({ code = 5 }) end,
		["errors-later"] = function() task.wait() error("failed after a wait") end,
		echo = function(...) return ... end,
	}
end
local CASES = { "several", "none", "table", "yields", "errors", "errors-plain", "errors-table", "errors-later", "echo" }
--- server
local Player = P.player()
local Shared = Instance.new("Part")
Shared.Name = "Shared"
Shared.Parent = workspace
local Mine = Instance.new("Folder")
Mine.Name = "ServerOnly"
Mine.Parent = game:GetService("ServerStorage")
local Functions = {}
for Name, Callback in Callbacks({ Shared = Shared, Mine = Mine }) do
	local Function = Instance.new("RemoteFunction")
	Function.Name = Name
	Function.OnServerInvoke = function(From, ...)
		assert(From == Player, "the caller's player first")
		return Callback(...)
	end
	Function.Parent = Storage
	Functions[Name] = Function
end
local Swap = Instance.new("RemoteFunction")
Swap.Name = "Swap"
Swap.Parent = Storage
local Order = Instance.new("RemoteFunction")
Order.Name = "Order"
Order.OnServerInvoke = function(_, Wait)
	task.wait(Wait)
	return Wait
end
Order.Parent = Storage
local Nested = Instance.new("RemoteFunction")
Nested.Name = "Nested"
Nested.Parent = Storage
Nested.OnServerInvoke = function(From, Value)
	return "server saw " .. Value .. " and the client said " .. tostring(Nested:InvokeClient(From, Value + 1))
end
P.meet("ready")
P.meet("client called")
-- the server calls the client
for _, Name in CASES do
	print("InvokeClient " .. Name .. " " .. Call(Functions[Name].InvokeClient, Functions[Name], Player, "a", nil))
end
P.meet("server called")
-- a callback set while a call waits for one, then replaced while another waits in it
Swap.OnServerInvoke = function()
	task.wait(0.2)
	return "first"
end
P.meet("swap set")
task.wait(0.05)
Swap.OnServerInvoke = function()
	return "second"
end
P.meet("swapped")
--- client
local Shared = workspace:WaitForChild("Shared")
local Functions = {}
for Name, Callback in Callbacks({ Shared = Shared, Mine = Instance.new("Folder") }) do
	local Function = Storage:WaitForChild(Name)
	Function.OnClientInvoke = Callback
	Functions[Name] = Function
end
local Swap = Storage:WaitForChild("Swap")
local Order = Storage:WaitForChild("Order")
local Nested = Storage:WaitForChild("Nested")
Nested.OnClientInvoke = function(Value)
	return "client saw " .. Value
end
P.meet("ready")
for _, Name in CASES do
	print("InvokeServer " .. Name .. " " .. Call(Functions[Name].InvokeServer, Functions[Name], "a", nil))
end
-- concurrent calls finishing in the reverse order
local Finished = {}
for _, Wait in { 0.3, 0.2, 0.1 } do
	task.spawn(function()
		table.insert(Finished, Order:InvokeServer(Wait))
	end)
end
task.wait(0.6)
print("concurrent finished " .. table.concat(Finished, " "))
print("nested " .. Call(Nested.InvokeServer, Nested, 1))
P.meet("client called")
P.meet("server called")
local Results = {}
task.spawn(function()
	table.insert(Results, "early " .. Call(Swap.InvokeServer, Swap))
end)
P.meet("swap set")
task.spawn(function()
	table.insert(Results, "during " .. Call(Swap.InvokeServer, Swap))
end)
P.meet("swapped")
task.spawn(function()
	table.insert(Results, "after " .. Call(Swap.InvokeServer, Swap))
end)
task.wait(0.5)
table.sort(Results)
print("swap " .. table.concat(Results, " | "))
