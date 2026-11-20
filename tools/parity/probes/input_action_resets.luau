-- The Input Action System: where Fire acts, what resets an action, a Scriptable binding's Fire.
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
	Action.StateChanged:Connect(function(Value)
		table.insert(Log, Label .. " StateChanged " .. fmt(Value))
	end)
	Action.Pressed:Connect(function()
		table.insert(Log, Label .. " Pressed")
	end)
	Action.Released:Connect(function()
		table.insert(Log, Label .. " Released")
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
local function fire(Label, Action, Value)
	print(Label .. " " .. P.try(function()
		Action:Fire(Value)
		return fmt(Action:GetState())
	end))
	flush(Label)
end

-- where Fire acts
do
	local Action = Instance.new("InputAction")
	Action.Parent = PlayerGui
	watch("in-playergui", Action)
	fire("in-playergui", Action, true)
	local Folder = Instance.new("Folder")
	local Under = Instance.new("InputAction")
	Under.Parent = Folder
	watch("under-folder", Under)
	fire("under-folder", Under, true)
	local Context = Instance.new("InputContext")
	local Deep = Instance.new("Folder")
	Deep.Parent = Context
	local Nested = Instance.new("InputAction")
	Nested.Parent = Deep
	watch("context-folder", Nested)
	fire("context-folder", Nested, true)
	local Nested2 = Instance.new("InputAction")
	local Inner = Instance.new("InputAction")
	Inner.Parent = Nested2
	Nested2.Parent = Context
	watch("action-in-action", Inner)
	fire("action-in-action", Inner, true)
end

-- resets
local function pressed(Label, Type, Value)
	local Context, Action = make(Type, PlayerGui)
	watch(Label, Action)
	Action:Fire(Value)
	flush(Label .. " fire")
	return Context, Action
end
do
	local Context, Action = pressed("disable-1d", Enum.InputActionType.Direction1D, 0.5)
	Context.Enabled = false
	print("disable-1d state " .. fmt(Action:GetState()))
	flush("disable-1d")
end
do
	local Context, Action = pressed("disable-vp", Enum.InputActionType.ViewportPosition, Vector2.new(10, 20))
	Context.Enabled = false
	print("disable-vp state " .. fmt(Action:GetState()))
	flush("disable-vp")
end
do
	local Context, Action = pressed("disable-2d", Enum.InputActionType.Direction2D, Vector2.new(0.5, 0))
	Action.Enabled = false
	print("action-disable-2d state " .. fmt(Action:GetState()))
	flush("action-disable-2d")
end
do
	local Context, Action = pressed("action-disable", nil, true)
	Action.Enabled = false
	print("action-disable state " .. fmt(Action:GetState()))
	flush("action-disable")
end
do
	local Context, Action = pressed("move-ctx", nil, true)
	Context.Parent = game:GetService("ReplicatedStorage")
	print("move-ctx state " .. fmt(Action:GetState()))
	flush("move-ctx")
end
do
	local Context, Action = pressed("move-ctx-nil-first", nil, true)
	local Other = Instance.new("InputContext")
	Action.Parent = Other
	print("move-action-to-nil-context state " .. fmt(Action:GetState()))
	flush("move-action-to-nil-context")
	Other.Parent = PlayerGui
	Action:Fire(true)
	flush("refire")
	local Third = Instance.new("InputContext")
	Third.Parent = PlayerGui
	Action.Parent = Third
	print("move-action-between state " .. fmt(Action:GetState()))
	flush("move-action-between")
end
do
	local Context, Action = pressed("type-change", nil, true)
	Action.Type = Enum.InputActionType.Direction1D
	print("type-change state " .. fmt(Action:GetState()))
	flush("type-change")
end
do
	local Context, Action = pressed("destroy-ctx", nil, true)
	Context:Destroy()
	print("destroy-ctx state " .. fmt(Action:GetState()))
	flush("destroy-ctx")
end
do
	local Context, Action = pressed("priority-change", nil, true)
	Context.Priority = 5
	Context.Sink = true
	print("priority-change state " .. fmt(Action:GetState()))
	flush("priority-change")
end
do
	-- a context made under a nil parent, then parented, while pressed
	local Context, Action = make(nil, nil)
	watch("nil-then-parent", Action)
	Action:Fire(true)
	flush("nil-then-parent fire")
	Context.Parent = PlayerGui
	print("nil-then-parent state " .. fmt(Action:GetState()))
	flush("nil-then-parent")
end

-- a Scriptable binding's Fire
local function bound(Type, Props)
	local Context, Action = make(Type, PlayerGui)
	local Binding = Instance.new("InputBinding")
	Binding.Type = Enum.InputBindingType.Scriptable
	for Key, Value in Props or {} do
		Binding[Key] = Value
	end
	Binding.Parent = Action
	return Action, Binding, Context
end
local function series(Label, Type, Props, Values)
	local Action, Binding = bound(Type, Props)
	watch(Label, Action)
	for _, Value in Values do
		print(Label .. " fire " .. fmt(Value) .. " " .. P.try(function()
			Binding:Fire(Value)
			return fmt(Action:GetState())
		end))
		flush(Label .. " " .. fmt(Value))
	end
	print(Label .. " fire-nil " .. P.try(function()
		Binding:Fire(nil)
		return fmt(Action:GetState())
	end))
end
series("bool", nil, nil, { true, true, false, 0.7, 0.3, 0.6, 0.1, 1, 0, -1, Vector2.new(1, 0), Vector2.new(0.3, 0.4), Vector3.new(1, 0, 0), "x", false })
series("thresh", nil, { PressedThreshold = 0.8, ReleasedThreshold = 0.4 }, { 0.7, 0.85, 0.5, 0.3, 0.9, false, true, 0.39 })
series("1d", Enum.InputActionType.Direction1D, nil, { 0.25, true, false, -0.3, 2, Vector2.new(0.5, 0.2), Vector3.new(0.1, 0.2, 0.3), 0.05, 0 })
series("1d-scale", Enum.InputActionType.Direction1D, { Scale = 2 }, { 0.25, 0.6, true, 0 })
series("1d-curve", Enum.InputActionType.Direction1D, { ResponseCurve = 2 }, { 0.5, -0.5, 1, 0.3 })
series("1d-dead", Enum.InputActionType.Direction1D, { Deadzone = NumberRange.new(0.2, 0.8) }, { 0.1, 0.5, 0.9, -0.5, 0.2, 0 })
series("2d", Enum.InputActionType.Direction2D, nil, { Vector2.new(0.3, 0.4), Vector2.new(3, 4), Vector2.new(0.05, 0), 0.5, true, Vector3.new(1, 2, 3) })
series("2d-scale", Enum.InputActionType.Direction2D, { Vector2Scale = Vector2.new(2, -1) }, { Vector2.new(0.3, 0.4), Vector2.new(0.8, 0) })
series("2d-noclamp", Enum.InputActionType.Direction2D, { ClampMagnitudeToOne = false }, { Vector2.new(3, 4) })
series("3d", Enum.InputActionType.Direction3D, nil, { Vector3.new(1, 2, 2), Vector3.new(0.05, 0, 0), Vector2.new(1, 0), 1, true })
series("3d-scale", Enum.InputActionType.Direction3D, { Vector3Scale = Vector3.new(1, 2, 3) }, { Vector3.new(0.1, 0.1, 0.1) })
series("vp", Enum.InputActionType.ViewportPosition, nil, { Vector2.new(100, 200), Vector2.new(0.5, 0.5), 1, true })
do
	-- two scriptable bindings, a binding beside an action fire
	local Action, First = bound(nil)
	local Second = Instance.new("InputBinding")
	Second.Type = Enum.InputBindingType.Scriptable
	Second.Parent = Action
	watch("two", Action)
	First:Fire(true)
	flush("two first-down")
	Second:Fire(true)
	flush("two second-down")
	First:Fire(false)
	print("two first-up state " .. fmt(Action:GetState()))
	flush("two first-up")
	Second:Fire(false)
	flush("two second-up")
end
do
	local Binding = Instance.new("InputBinding")
	Binding.Type = Enum.InputBindingType.Scriptable
	print("orphan " .. P.try(function()
		Binding:Fire(true)
		return "done"
	end))
	local Action = Instance.new("InputAction")
	Binding.Parent = Action
	watch("no-ctx", Action)
	print("no-ctx " .. P.try(function()
		Binding:Fire(true)
		return fmt(Action:GetState())
	end))
	flush("no-ctx")
	local Folder = Instance.new("Folder")
	local Context = Instance.new("InputContext")
	Action.Parent = Context
	Folder.Parent = Action
	Binding.Parent = Folder
	Context.Parent = PlayerGui
	print("in-folder " .. P.try(function()
		Binding:Fire(true)
		return fmt(Action:GetState())
	end))
	flush("in-folder")
	local Disabled = Instance.new("InputContext")
	Disabled.Enabled = false
	Disabled.Parent = PlayerGui
	Action.Parent = Disabled
	Binding.Parent = Action
	print("ctx-disabled " .. P.try(function()
		Binding:Fire(true)
		return fmt(Action:GetState())
	end))
	flush("ctx-disabled")
end
