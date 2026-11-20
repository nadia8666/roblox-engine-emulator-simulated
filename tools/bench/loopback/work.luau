-- The loopback bench's workloads (tests/loopback_bench.rs): each run of this text replaces --WORKLOAD-- with one call. The
-- server's side makes the remotes in ReplicatedStorage; the client's finds them once they replicated (the bench steps two
-- frames before it times any).
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
local CALLS = 1000
local Array100 = table.create(100, 1.5)
local Array1000 = table.create(1000, 1.5)
local Buffer1K = buffer.create(1024)

local function Make(Class, Name)
	local Made = Instance.new(Class)
	Made.Name = Name
	Made.Parent = Storage
	return Made
end

-- the server: a remote whose handler counts what arrives
function Handler()
	local Count = 0
	Make("RemoteEvent", "BenchRemote").OnServerEvent:Connect(function()
		Count += 1
	end)
end

-- the client: CALLS FireServer calls a frame (no arguments, or one)
function Fire(Argument)
	RunService.Heartbeat:Connect(function()
		local Remote = Storage:FindFirstChild("BenchRemote")
		if not Remote then
			return
		end
		if Argument == nil then
			for _ = 1, CALLS do
				Remote:FireServer()
			end
		else
			for _ = 1, CALLS do
				Remote:FireServer(Argument)
			end
		end
	end)
end

-- a plain client: CALLS BindableEvent:Fire calls a frame, one handler
function Bindable()
	local Event = Instance.new("BindableEvent")
	local Count = 0
	Event.Event:Connect(function()
		Count += 1
	end)
	RunService.Heartbeat:Connect(function()
		for _ = 1, CALLS do
			Event:Fire()
		end
	end)
end

-- the server: a RemoteFunction answering at once
function Callback()
	Make("RemoteFunction", "BenchCall").OnServerInvoke = function()
		return nil
	end
end

-- the client: Count invokes, one after another
function Invoke(Count)
	task.spawn(function()
		local Call = Storage:WaitForChild("BenchCall")
		for _ = 1, Count do
			Call:InvokeServer()
		end
	end)
end

-- the server: 2000 IntValues in a container, each written once a frame
function Write(Container)
	local Folder = Instance.new("Folder")
	Folder.Name = "BenchValues"
	local Values = table.create(2000)
	for Index = 1, 2000 do
		local Value = Instance.new("IntValue")
		Value.Parent = Folder
		Values[Index] = Value
	end
	Folder.Parent = game:GetService(Container)
	local Frame = 0
	RunService.Heartbeat:Connect(function()
		Frame += 1
		for _, Value in Values do
			Value.Value = Frame
		end
	end)
end

--WORKLOAD--
