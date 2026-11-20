-- parity: loopback
-- T9: what the client sees of several server writes in one frame: three writes to one property, a write and its revert, a
-- property written after the instance was parented, an attribute written twice, a new child written after parenting, and
-- writes to two properties of one instance (in which order their Changed run). Each case runs in a frame of its own.
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
--- server
P.player()
local Box = Instance.new("Folder")
Box.Name = "Box"
Box.Parent = Storage
local Value = Instance.new("IntValue")
Value.Name = "Value"
Value.Parent = Box
local Part = Instance.new("Part")
Part.Name = "Part"
Part.Parent = Box
P.meet("ready")
local function Frame(Step)
	local Done = false
	RunService.Heartbeat:Once(function()
		Step()
		Done = true
	end)
	while not Done do
		task.wait()
	end
	for _ = 1, 5 do
		task.wait()
	end
end
Frame(function()
	Value.Value = 1
	Value.Value = 2
	Value.Value = 3
end)
P.meet("three")
P.meet("three printed")
Frame(function()
	Value.Value = 10
	Value.Value = 3
end)
P.meet("revert")
P.meet("revert printed")
Frame(function()
	local Child = Instance.new("IntValue")
	Child.Name = "Child"
	Child.Value = 4
	Child.Parent = Box
	Child.Value = 5
	Child:SetAttribute("A", 1)
end)
P.meet("child")
P.meet("child printed")
Frame(function()
	Value:SetAttribute("Twice", 1)
	Value:SetAttribute("Twice", 2)
end)
P.meet("attribute")
P.meet("attribute printed")
Frame(function()
	Part.Transparency = 0.5
	Part.Reflectance = 0.5
	Part.Transparency = 0.75
end)
P.meet("two")
P.meet("two printed")
--- client
local Box = Storage:WaitForChild("Box")
local Value = Box:WaitForChild("Value")
local Part = Box:WaitForChild("Part")
local Seen = {}
Value.Changed:Connect(function(New)
	table.insert(Seen, "Changed " .. P.num(New) .. " reads " .. P.num(Value.Value))
end)
Value:GetPropertyChangedSignal("Value"):Connect(function()
	table.insert(Seen, "ValueChanged reads " .. P.num(Value.Value))
end)
Value.AttributeChanged:Connect(function(Name)
	table.insert(Seen, "AttributeChanged " .. Name .. " reads " .. tostring(Value:GetAttribute(Name)))
end)
Box.ChildAdded:Connect(function(Child)
	if Child:IsA("IntValue") then
		table.insert(Seen, "ChildAdded " .. Child.Name .. " reads " .. P.num(Child.Value) .. " attribute " .. tostring(Child:GetAttribute("A")))
		Child.Changed:Connect(function(New)
			table.insert(Seen, "child Changed " .. P.num(New))
		end)
	end
end)
Part.Changed:Connect(function(Name)
	if Name == "Transparency" or Name == "Reflectance" then
		table.insert(Seen, "Part " .. Name .. " " .. P.num(Part[Name]))
	end
end)
P.meet("ready")
for _, Case in { "three", "revert", "child", "attribute", "two" } do
	P.meet(Case)
	print(Case .. ": " .. table.concat(Seen, " / "))
	table.clear(Seen)
	P.meet(Case .. " printed")
end
