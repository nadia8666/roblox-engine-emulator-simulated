-- AssetService:CreateDataModelContentAsync: what it answers, what takes its Content, what it refuses (a playing client).
local AssetService = game:GetService("AssetService")
local function show(Value)
	local Kind = typeof(Value)
	if Kind == "Content" then
		return "Content(" .. tostring(Value.SourceType) .. " uri=" .. tostring(Value.Uri) .. " object=" .. typeof(Value.Object) .. ")"
	elseif Kind == "Vector3" then
		return "Vector3(" .. P.f32s(Value.X, Value.Y, Value.Z) .. ")"
	elseif Kind == "number" then
		return P.num(Value)
	end
	return Kind .. ":" .. tostring(Value)
end
local function try(Label, Function)
	local Results = table.pack(pcall(Function))
	if Results[1] then
		local Out = { "n=" .. (Results.n - 1) }
		for Index = 2, Results.n do
			table.insert(Out, show(Results[Index]))
		end
		print(Label .. " ok " .. table.concat(Out, " "))
	else
		print(Label .. " error " .. P.err(Results[2]))
	end
end
local Triangle = AssetService:CreateEditableMesh()
do
	local A = Triangle:AddVertex(Vector3.new(0, 0, 0))
	local B = Triangle:AddVertex(Vector3.new(2, 0, 0))
	local C = Triangle:AddVertex(Vector3.new(0, 3, 0))
	Triangle:AddTriangle(A, B, C)
end
P.sync()
local Count, Connection = P.frames()
local Deferred = false
task.defer(function()
	Deferred = true
end)
local Result, Made = AssetService:CreateDataModelContentAsync(Content.fromObject(Triangle))
Connection:Disconnect()
print("yields " .. tostring(Count() > 0) .. " " .. tostring(Deferred))
print("made " .. show(Result) .. " " .. show(Made) .. " " .. tostring(Made) .. " " .. tostring(Made == Made))
try("options", function()
	return AssetService:CreateDataModelContentAsync(Content.fromObject(Triangle), {})
end)
try("again", function()
	local _, Again = AssetService:CreateDataModelContentAsync(Content.fromObject(Triangle))
	return Again == Made
end)
try("image", function()
	return AssetService:CreateDataModelContentAsync(Content.fromObject(AssetService:CreateEditableImage({ Size = Vector2.new(4, 4) })))
end)
for _, Case in {
	{ "uri", Content.fromUri("rbxassetid://1") },
	{ "none", Content.none },
	{ "string", "rbxassetid://1" },
	{ "empty-mesh", Content.fromObject(AssetService:CreateEditableMesh()) },
} do
	try(Case[1], function()
		return AssetService:CreateDataModelContentAsync(Case[2])
	end)
end
try("mesh-part", function()
	local Part = AssetService:CreateMeshPartAsync(Made)
	return Part.ClassName, Part.MeshSize, Part.Size
end)
try("editable-copy", function()
	local Copy = AssetService:CreateEditableMeshAsync(Made)
	return #Copy:GetVertices(), #Copy:GetFaces()
end)
try("after-edit", function()
	Triangle:SetPosition(Triangle:GetVertices()[1], Vector3.new(-1, 0, 0))
	local Copy = AssetService:CreateEditableMeshAsync(Made)
	return Copy:GetPosition(Copy:GetVertices()[1]), Copy:GetPosition(Copy:GetVertices()[3])
end)
try("after-destroy", function()
	Triangle:Destroy()
	return AssetService:CreateMeshPartAsync(Made).MeshSize
end)
