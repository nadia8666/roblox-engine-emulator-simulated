-- A Content made by Content.fromObject keeps its object: an EditableImage only the Content holds is still there after the
-- collector had every chance to run, and can be shown (a playing client).
local AssetService = game:GetService("AssetService")
local Kept
do
	Kept = Content.fromObject(AssetService:CreateEditableImage({ Size = Vector2.new(8, 8) }))
end
for _ = 1, 60 do
	local Garbage = {}
	for Index = 1, 20000 do
		Garbage[Index] = { Index }
	end
	task.wait()
end
print("kept " .. tostring(Kept.Object ~= nil) .. " " .. P.try(function()
	return Kept.Object.ClassName
end))
print("shown " .. P.try(function()
	local Label = Instance.new("ImageLabel")
	Label.ImageContent = Kept
	return "assigned"
end))
