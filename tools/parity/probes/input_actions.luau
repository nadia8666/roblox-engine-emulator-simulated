-- The Input Action System from scripts: GetState, Fire, signals, where Fire acts, types (a playing client).
local Players = game:GetService("Players")
local PlayerGui = Players.LocalPlayer:WaitForChild("PlayerGui")
local Log = {}
local function fmt(Value)
	local Kind = typeof(Value)
	if Kind == "number" then
		return "number:" .. P.num(Value)
	elseif Kind == "Vector2" then
		return "Vector2:" .. P.f32s(Value.X, Value.Y)
	elseif Kind == "Vector3" then
		return "Vector3:" .. P.f32s(Value.X, Value.Y, Value.Z)
	end
	return Kind .. ":" .. tostring(Value)
end
local function watch(Label, Action)
	Action.StateChanged:Connect(function(...)
		local N = select("#", ...)
		table.insert(Log, Label .. " StateChanged(" .. N .. ") " .. fmt((...)) .. " now " .. fmt(Action:GetState()))
	end)
	Action.Pressed:Connect(function(...)
		table.insert(Log, Label .. " Pressed(" .. select("#", ...) .. ") now " .. fmt(Action:GetState()))
	end)
	Action.Released:Connect(function(...)
		table.insert(Log, Label .. " Released(" .. select("#", ...) .. ") now " .. fmt(Action:GetState()))
	end)
end
local function flush(Label)
	task.wait()
	task.wait()
	print(Label .. ": " .. (#Log == 0 and "(nothing)" or table.concat(Log, " | ")))
	table.clear(Log)
end
local function make(Type, Parent)
	local Context = Instance.new("InputContext")
	local Action = Instance.new("InputAction")
	if Type then
		Action.Type = Type
	end
	Action.Parent = Context
	Context.Parent = Parent
	return Context, Action
end

-- defaults
for _, Type in Enum.InputActionType:GetEnumItems() do
	local Action = Instance.new("InputAction")
	Action.Type = Type
	print("default " .. Type.Name .. " " .. fmt(Action:GetState()))
end
print("default-type " .. tostring(Instance.new("InputAction").Type))

-- where a context must be for Fire to act
local Places = {
	{ "no-context" },
	{ "context-nil", false },
	{ "ReplicatedStorage", game:GetService("ReplicatedStorage") },
	{ "Workspace", workspace },
	{ "PlayerGui", PlayerGui },
	{ "LocalPlayer", Players.LocalPlayer },
	{ "StarterGui", game:GetService("StarterGui") },
	{ "Lighting", game:GetService("Lighting") },
}
for _, Place in Places do
	local Name, Parent = Place[1], Place[2]
	local Action
	if Parent == nil then
		Action = Instance.new("InputAction")
	else
		_, Action = make(nil, Parent or nil)
	end
	watch(Name, Action)
	print("fire " .. Name .. " " .. P.try(function()
		Action:Fire(true)
		return fmt(Action:GetState()), #Log
	end))
	task.defer(function()
		table.insert(Log, "defer-marker")
	end)
	flush("after " .. Name)
	print("fire-false " .. Name .. " " .. P.try(function()
		Action:Fire(false)
		return fmt(Action:GetState()), #Log
	end))
	flush("after-false " .. Name)
end

-- enabled flags
do
	local Context, Action = make(nil, PlayerGui)
	watch("ctx-off", Action)
	Context.Enabled = false
	print("ctx-off fire " .. P.try(function()
		Action:Fire(true)
		return fmt(Action:GetState())
	end))
	flush("ctx-off")
	Context.Enabled = true
	print("ctx-on-again state " .. fmt(Action:GetState()))
	flush("ctx-on-again")
	Action:Fire(true)
	flush("ctx-on fire")
	Context.Enabled = false
	print("ctx-disabled-while-pressed state " .. fmt(Action:GetState()))
	flush("ctx-disabled-while-pressed")
	print("ctx-disabled-while-pressed later " .. fmt(Action:GetState()))
end
do
	local Context, Action = make(nil, PlayerGui)
	watch("act-off", Action)
	print("act-enabled-write " .. P.try(function()
		Action.Enabled = false
		return Action.Enabled
	end))
	print("act-off fire " .. P.try(function()
		Action:Fire(true)
		return fmt(Action:GetState())
	end))
	flush("act-off")
	Action.Enabled = true
	print("act-on-again state " .. fmt(Action:GetState()))
	flush("act-on-again")
end
do
	-- reparenting a pressed action's context away
	local Context, Action = make(nil, PlayerGui)
	watch("moved", Action)
	Action:Fire(true)
	flush("moved fire")
	Context.Parent = nil
	print("moved state " .. fmt(Action:GetState()))
	flush("moved away")
	Context.Parent = PlayerGui
	print("moved back state " .. fmt(Action:GetState()))
	flush("moved back")
end

-- each type: values, repeats, wrong values
local Cases = {
	Bool = { true, true, false, false, 1, 0, "x", Vector2.new(1, 0) },
	Direction1D = { 0.7, 0.7, 0.3, 0, -0.7, 2, -3, true, false, Vector2.new(1, 0), nil, 0 },
	Direction2D = { Vector2.new(0.6, 0.8), Vector2.new(3, 4), Vector2.zero, Vector3.new(1, 2, 3), 1, true },
	Direction3D = { Vector3.new(1, 2, 2), Vector3.new(0.1, 0, 0), Vector3.zero, Vector2.new(1, 1), 1 },
	ViewportPosition = { Vector2.new(100, 200), Vector2.new(-5, 5000), Vector3.new(1, 2, 3), 1 },
}
for _, Type in Enum.InputActionType:GetEnumItems() do
	local _, Action = make(Type, PlayerGui)
	watch(Type.Name, Action)
	for _, Value in Cases[Type.Name] do
		print(Type.Name .. " fire " .. fmt(Value) .. " " .. P.try(function()
			Action:Fire(Value)
			return fmt(Action:GetState())
		end))
		flush(Type.Name .. " " .. fmt(Value))
	end
	print(Type.Name .. " fire-nil " .. P.try(function()
		Action:Fire(nil)
		return fmt(Action:GetState())
	end))
	flush(Type.Name .. " nil")
	print(Type.Name .. " fire-noarg " .. P.try(function()
		Action:Fire()
		return fmt(Action:GetState())
	end))
	flush(Type.Name .. " noarg")
end

-- order within a frame: two fires before a yield
do
	local _, Action = make(nil, PlayerGui)
	watch("twice", Action)
	Action:Fire(true)
	Action:Fire(false)
	print("twice state " .. fmt(Action:GetState()))
	flush("twice")
	local _, Action1 = make(Enum.InputActionType.Direction1D, PlayerGui)
	watch("twice1d", Action1)
	Action1:Fire(0.4)
	Action1:Fire(0.9)
	Action1:Fire(0.2)
	flush("twice1d")
end

-- bindings
local function bound(Type, Props)
	local Context, Action = make(Type, PlayerGui)
	local Binding = Instance.new("InputBinding")
	for Key, Value in Props or {} do
		Binding[Key] = Value
	end
	Binding.Parent = Action
	return Action, Binding, Context
end
do
	local Action, Binding = bound(nil)
	watch("bind-bool", Action)
	for _, Value in { true, false, 0.7, 0.3, 0.1, 1, 0, Vector2.new(1, 0), "x" } do
		print("bind-bool fire " .. fmt(Value) .. " " .. P.try(function()
			Binding:Fire(Value)
			return fmt(Action:GetState())
		end))
		flush("bind-bool " .. fmt(Value))
	end
end
do
	local Action, Binding = bound(Enum.InputActionType.Direction1D, { Scale = 2 })
	watch("bind-1d", Action)
	for _, Value in { 0.25, 0.05, 0.6, true, false, -0.3, 0 } do
		print("bind-1d fire " .. fmt(Value) .. " " .. P.try(function()
			Binding:Fire(Value)
			return fmt(Action:GetState())
		end))
		flush("bind-1d " .. fmt(Value))
	end
end
do
	local Action, Binding = bound(Enum.InputActionType.Direction1D, { ResponseCurve = 2 })
	watch("bind-1d-curve", Action)
	for _, Value in { 0.5, 0.05, 1 } do
		print("bind-1d-curve fire " .. fmt(Value) .. " " .. P.try(function()
			Binding:Fire(Value)
			return fmt(Action:GetState())
		end))
		flush("bind-1d-curve " .. fmt(Value))
	end
end
do
	local Action, Binding = bound(Enum.InputActionType.Direction2D, { Vector2Scale = Vector2.new(2, -1) })
	watch("bind-2d", Action)
	for _, Value in { Vector2.new(0.3, 0.4), Vector2.new(0.05, 0), Vector2.new(3, 4), 0.5, true } do
		print("bind-2d fire " .. fmt(Value) .. " " .. P.try(function()
			Binding:Fire(Value)
			return fmt(Action:GetState())
		end))
		flush("bind-2d " .. fmt(Value))
	end
end
do
	local Action, Binding = bound(Enum.InputActionType.Bool, { PressedThreshold = 0.8, ReleasedThreshold = 0.4 })
	watch("bind-thresh", Action)
	for _, Value in { 0.7, 0.85, 0.5, 0.3, 0.9, false } do
		print("bind-thresh fire " .. fmt(Value) .. " " .. P.try(function()
			Binding:Fire(Value)
			return fmt(Action:GetState())
		end))
		flush("bind-thresh " .. fmt(Value))
	end
end
print("binding-type-default " .. tostring(Instance.new("InputBinding").Type))
