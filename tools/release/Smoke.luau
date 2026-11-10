-- The harness of tools/release/smoke.mjs: it needs no Studio, snapshot or network, and
-- prints "release smoke 1,2,3" when everything it checks held.
return function(Context)
	-- a standalone run has no implicit project or snapshot to supply instances
	local Storage = game:GetService("ReplicatedStorage")
	assert(Storage:FindFirstChild("Unrequested") == nil, "no implicit project or snapshot")
	-- pixel.png sits in the current folder, which is the project
	local AssetService = game:GetService("AssetService")
	local Image = AssetService:CreateEditableImageAsync(Content.fromUri("pixel.png"))
	assert(Image.Size == Vector2.one, "relative content comes from the current project")
	Image:DrawRectangle(Vector2.zero, Vector2.one, Color3.new(1, 0, 0), 0, Enum.ImageCombineType.Overwrite)
	local Surface = AssetService:CreateSurfaceAppearanceAsync({ ColorMap = Content.fromObject(Image) })
	local Part = Instance.new("MeshPart")
	Part.Name = "ReleaseSmoke"
	Part.Anchored = true
	Surface.Parent = Part
	Part.Parent = workspace
	-- plain Luau, to see that the script itself ran
	local Values = { 3, 1, 2 }
	table.sort(Values)
	assert(table.concat(Values, ",") == "1,2,3")
	task.wait()
	print("release smoke 1,2,3")
end
