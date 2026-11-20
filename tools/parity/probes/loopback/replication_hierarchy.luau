-- parity: loopback
-- T10: what the client's copy of an instance does when the server unparents it, parents it again, moves it out of the
-- replicated containers and back, and destroys it: its Parent, whether the client's reference stays the same instance,
-- and which of its events fire there (AncestryChanged, ChildRemoved, Destroying).
local Storage = game:GetService("ReplicatedStorage")
--- server
P.player()
local Box = Instance.new("Folder")
Box.Name = "Box"
Box.Parent = Storage
local Item = Instance.new("Folder")
Item.Name = "Item"
Item.Parent = Box
local Inner = Instance.new("Folder")
Inner.Name = "Inner"
Inner.Parent = Item
P.meet("ready")
Item.Parent = nil
P.meet("unparented")
P.meet("unparented seen")
Item.Parent = Box
P.meet("reparented")
P.meet("reparented seen")
Item.Parent = game:GetService("ServerStorage")
P.meet("hidden")
P.meet("hidden seen")
Item.Parent = Box
P.meet("shown")
P.meet("shown seen")
Item:Destroy()
P.meet("destroyed")
P.meet("destroyed seen")
--- client
local Box = Storage:WaitForChild("Box")
local Item = Box:WaitForChild("Item")
local Inner = Item:WaitForChild("Inner")
local Events = {}
Item.AncestryChanged:Connect(function(_, Parent)
	table.insert(Events, "AncestryChanged " .. tostring(Parent))
end)
Item.Destroying:Connect(function()
	table.insert(Events, "Destroying")
end)
Inner.Destroying:Connect(function()
	table.insert(Events, "Inner Destroying")
end)
Box.ChildRemoved:Connect(function(Child)
	table.insert(Events, "ChildRemoved " .. Child.Name)
end)
Box.ChildAdded:Connect(function(Child)
	table.insert(Events, "ChildAdded " .. Child.Name .. " same " .. tostring(Child == Item))
end)
P.meet("ready")
for _, Step in { "unparented", "reparented", "hidden", "shown", "destroyed" } do
	P.meet(Step)
	for _ = 1, 5 do
		task.wait()
	end
	local Found = Box:FindFirstChild("Item")
	print(Step .. ": parent " .. tostring(Item.Parent) .. " found " .. tostring(Found ~= nil) .. " same " .. tostring(Found == Item)
		.. " inner " .. tostring(Inner.Parent) .. " events " .. table.concat(Events, " / "))
	table.clear(Events)
	P.meet(Step .. " seen")
end
