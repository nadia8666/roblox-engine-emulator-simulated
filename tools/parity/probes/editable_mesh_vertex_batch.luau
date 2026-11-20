-- EditableMesh's per-vertex batches and BatchRemove: BatchGetVertexAttributes, BatchGetVertexFaceAttributes,
-- BatchSetVertexFaceAttributes and BatchRemove, beside the single-item methods they should agree with (a playing client).
local AssetService = game:GetService("AssetService")
local function show(Value)
	local Kind = typeof(Value)
	if Kind == "number" then
		if Value > 4294967296 then
			return string.format("%d:%d", Value // 4294967296, Value % 4294967296)
		end
		return P.num(Value)
	elseif Kind == "Vector3" then
		return "V3(" .. P.f32s(Value.X, Value.Y, Value.Z) .. ")"
	elseif Kind == "Vector2" then
		return "V2(" .. P.f32s(Value.X, Value.Y) .. ")"
	elseif Kind == "Color3" then
		return "C3(" .. P.f32s(Value.R, Value.G, Value.B) .. ")"
	elseif Kind == "table" then
		local Parts = {}
		for Key, Item in Value do
			table.insert(Parts, (if type(Key) == "number" and Key == #Parts + 1 then "" else tostring(Key) .. "=") .. show(Item))
		end
		return "{" .. table.concat(Parts, ",") .. "}"
	end
	return Kind .. ":" .. tostring(Value)
end
local Made = {}
local function try(Label, Function)
	local Results = table.pack(pcall(Function))
	for _, Mesh in Made do
		Mesh:Destroy()
	end
	table.clear(Made)
	if Results[1] then
		local Out = {}
		for Index = 2, Results.n do
			table.insert(Out, show(Results[Index]))
		end
		print(Label .. " ok " .. table.concat(Out, " "))
	else
		print(Label .. " error " .. P.err(Results[2]))
	end
end
local function fresh(Options)
	local Mesh = AssetService:CreateEditableMesh(Options)
	table.insert(Made, Mesh)
	local V = {}
	for _, Position in { Vector3.new(0, 0, 0), Vector3.new(1, 0, 0), Vector3.new(1, 1, 0), Vector3.new(0, 1, 0), Vector3.new(0, 0, 1), Vector3.new(2, 2, 2) } do
		table.insert(V, Mesh:AddVertex(Position))
	end
	return Mesh, V
end
-- Faces A (v1 v2 v3), B (v1 v3 v4), C (v2 v3 v5) made one by one; v6 is on no face. B's corner at v1 then gets a normal,
-- uv and colour of its own, so v1 has two of each.
local function built(Options)
	local Mesh, V = fresh(Options)
	local F = { Mesh:AddTriangle(V[1], V[2], V[3]), Mesh:AddTriangle(V[1], V[3], V[4]), Mesh:AddTriangle(V[2], V[3], V[5]) }
	local N = Mesh:AddNormal(Vector3.new(1, 0, 0))
	local U = Mesh:AddUV(Vector2.new(0.5, 0.5))
	local C = Mesh:AddColor(Color3.new(1, 0, 0), 1)
	local Normals, UVs, Colors = Mesh:GetFaceNormals(F[2]), Mesh:GetFaceUVs(F[2]), Mesh:GetFaceColors(F[2])
	Mesh:SetFaceNormals(F[2], { N, Normals[2], Normals[3] })
	Mesh:SetFaceUVs(F[2], { U, UVs[2], UVs[3] })
	Mesh:SetFaceColors(F[2], { C, Colors[2], Colors[3] })
	return Mesh, V, F, { N = N, U = U, C = C }
end
local function counts(Mesh)
	return #Mesh:GetVertices() .. "v " .. #Mesh:GetNormals() .. "n " .. #Mesh:GetUVs() .. "u " .. #Mesh:GetColors() .. "c " .. #Mesh:GetFaces() .. "f " .. #Mesh:GetBones() .. "b"
end
local function corners(Mesh, Face)
	return "v" .. show(Mesh:GetFaceVertices(Face)) .. " n" .. show(Mesh:GetFaceNormals(Face)) .. " u" .. show(Mesh:GetFaceUVs(Face)) .. " c" .. show(Mesh:GetFaceColors(Face))
end
local Kinds = { "Vertex", "Normal", "Color", "UV", "Face" }

-- BatchGetVertexAttributes, each kind, beside the single-item lists
for _, Kind in Kinds do
	try("vertex-attributes-" .. Kind, function()
		local Mesh, V = built()
		return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute[Kind], V)
	end)
end
try("vertex-singles", function()
	local Mesh, V = built()
	local Out = {}
	for Index, Vertex in V do
		table.insert(Out, "v" .. Index .. " f" .. show(Mesh:GetVertexFaces(Vertex)) .. " n" .. show(Mesh:GetVertexNormals(Vertex)) .. " u" .. show(Mesh:GetVertexUVs(Vertex)) .. " c" .. show(Mesh:GetVertexColors(Vertex)) .. " a" .. show(Mesh:GetAdjacentVertices(Vertex)))
	end
	return table.concat(Out, "; ")
end)
-- a corner given an attribute made before the one the first face has: the order is by face or by id
try("vertex-attributes-order", function()
	local Mesh, V = fresh()
	local Early = Mesh:AddNormal(Vector3.new(0, 1, 0))
	local A = Mesh:AddTriangle(V[1], V[2], V[3])
	local B = Mesh:AddTriangle(V[1], V[3], V[4])
	local Normals = Mesh:GetFaceNormals(B)
	Mesh:SetFaceNormals(B, { Early, Normals[2], Normals[3] })
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { V[1] }), Mesh:GetVertexNormals(V[1]), Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Face, { V[1] }), Mesh:GetVertexFaces(V[1]), A, B
end)
-- faces from BatchAdd (ids run backwards)
try("vertex-attributes-batchadd", function()
	local Mesh, V = fresh()
	local F = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3] }, { V[1], V[3], V[4] }, { V[1], V[4], V[5] } })
	local Normals = Mesh:GetFaceNormals(F[3])
	Mesh:SetFaceNormals(F[3], { Mesh:AddNormal(Vector3.xAxis), Normals[2], Normals[3] })
	Normals = Mesh:GetFaceNormals(F[1])
	Mesh:SetFaceNormals(F[1], { Mesh:AddNormal(Vector3.yAxis), Normals[2], Normals[3] })
	return F, Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Face, { V[1] }), Mesh:GetVertexFaces(V[1]), Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { V[1] }), Mesh:GetVertexNormals(V[1])
end)
try("vertex-attributes-twice-in-face", function()
	local Mesh, V = fresh()
	local F = Mesh:AddTriangle(V[1], V[1], V[2])
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { V[1], V[2] }), Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Face, { V[1] }), corners(Mesh, F)
end)
try("vertex-attributes-repeat", function()
	local Mesh, V = built()
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { V[3], V[3], V[6] })
end)
try("vertex-attributes-empty", function()
	local Mesh = built()
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, {})
end)
try("vertex-attributes-fixed", function()
	local Mesh, V = built()
	local Copy = AssetService:CreateEditableMeshAsync(Content.fromObject(Mesh), { FixedSize = true })
	table.insert(Made, Copy)
	return Copy.FixedSize, Copy:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, V), Copy:BatchGetVertexAttributes(Enum.MeshAttribute.Face, V)
end)
try("vertex-attributes-fixed-new", function()
	local Mesh, V = built({ FixedSize = true })
	return Mesh.FixedSize, Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.UV, V)
end)
try("vertex-attributes-unused-after-remove", function()
	local Mesh, V, F = built()
	Mesh:RemoveFace(F[3])
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { V[5], V[2] }), Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Face, { V[5], V[2] })
end)
for _, Case in {
	{ "gone", function(Mesh, V) Mesh:RemoveUnused() return { V[1], V[6] } end },
	{ "past", function(Mesh, V) return { V[1], V[6] + 100 } end },
	{ "normal-id", function(Mesh, V, F, X) return { V[1], X.N } end },
	{ "face-id", function(Mesh, V, F) return { F[1] } end },
	{ "no-id", function() return { 12345 } end },
	{ "zero", function() return { 0 } end },
	{ "string", function(Mesh, V) return { tostring(V[1]) } end },
	{ "word", function() return { "x" } end },
	{ "hole", function(Mesh, V) return { V[1], nil, V[2] } end },
	{ "keyed", function(Mesh, V) return { a = V[1] } end },
	{ "fraction", function(Mesh, V) return { V[1] + 0.5 } end },
} do
	try("vertex-attributes-" .. Case[1], function()
		local Mesh, V, F, X = built()
		local Ids = Case[2](Mesh, V, F, X)
		return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, Ids)
	end)
end
for _, Case in {
	{ "attribute-string", "Normal" },
	{ "attribute-number", 1 },
	{ "attribute-nil", nil },
	{ "attribute-other-enum", Enum.KeyCode.A },
} do
	try("vertex-attributes-" .. Case[1], function()
		local Mesh, V = built()
		return Mesh:BatchGetVertexAttributes(Case[2], V)
	end)
end
try("vertex-attributes-ids-number", function()
	local Mesh, V = built()
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, V[1])
end)
try("vertex-attributes-ids-nil", function()
	local Mesh = built()
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal)
end)
try("vertex-attributes-dot", function()
	local Mesh, V = built()
	return Mesh.BatchGetVertexAttributes(Mesh, Enum.MeshAttribute.Normal, { V[1] })
end)

-- BatchGetVertexFaceAttributes, each kind, beside the single-item corner getters
for _, Kind in Kinds do
	try("vertex-face-attributes-" .. Kind, function()
		local Mesh, V, F = built()
		return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute[Kind], { V[1], V[2], V[3], V[1], V[3], V[5] }, { F[1], F[1], F[1], F[2], F[2], F[3] })
	end)
end
try("vertex-face-singles", function()
	local Mesh, V, F = built()
	return Mesh:GetVertexFaceNormal(V[1], F[2]), Mesh:GetVertexFaceUV(V[1], F[2]), Mesh:GetVertexFaceColor(V[1], F[2]), Mesh:GetVertexFaceNormal(V[1], F[1])
end)
try("vertex-face-single-not-in-face", function()
	local Mesh, V, F = built()
	return Mesh:GetVertexFaceNormal(V[5], F[1])
end)
try("vertex-face-twice-in-face", function()
	local Mesh, V = fresh()
	local F = Mesh:AddTriangle(V[1], V[1], V[2])
	return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, { V[1], V[2] }, { F, F }), corners(Mesh, F)
end)
try("vertex-face-empty", function()
	local Mesh = built()
	return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, {}, {})
end)
try("vertex-face-fixed", function()
	local Mesh, V, F = built()
	local Copy = AssetService:CreateEditableMeshAsync(Content.fromObject(Mesh), { FixedSize = true })
	table.insert(Made, Copy)
	return Copy:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, { V[1], V[1] }, { F[1], F[2] })
end)
for _, Case in {
	{ "not-in-face", function(Mesh, V, F) return { V[1], V[5] }, { F[1], F[1] } end },
	{ "short-faces", function(Mesh, V, F) return { V[1], V[2] }, { F[1] } end },
	{ "short-vertices", function(Mesh, V, F) return { V[1] }, { F[1], F[1] } end },
	{ "gone-face", function(Mesh, V, F) Mesh:RemoveFace(F[3]) return { V[1], V[2] }, { F[1], F[3] } end },
	{ "gone-vertex", function(Mesh, V, F) Mesh:RemoveUnused() return { V[6] }, { F[1] } end },
	{ "swapped", function(Mesh, V, F) return { F[1] }, { V[1] } end },
	{ "normal-as-vertex", function(Mesh, V, F, X) return { X.N }, { F[2] } end },
	{ "no-id-vertex", function(Mesh, V, F) return { 12345 }, { F[1] } end },
	{ "no-id-face", function(Mesh, V, F) return { V[1] }, { 12345 } end },
	{ "past-face", function(Mesh, V, F) return { V[1] }, { F[3] + 100 } end },
	{ "word", function(Mesh, V, F) return { "x" }, { F[1] } end },
} do
	try("vertex-face-" .. Case[1], function()
		local Mesh, V, F, X = built()
		local Vertices, Faces = Case[2](Mesh, V, F, X)
		return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, Vertices, Faces)
	end)
end
try("vertex-face-attribute-nil", function()
	local Mesh, V, F = built()
	return Mesh:BatchGetVertexFaceAttributes(nil, { V[1] }, { F[1] })
end)
try("vertex-face-faces-nil", function()
	local Mesh, V = built()
	return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, { V[1] })
end)
try("vertex-face-vertices-number", function()
	local Mesh, V, F = built()
	return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, V[1], { F[1] })
end)

-- BatchSetVertexFaceAttributes
try("set-normal", function()
	local Mesh, V, F = built()
	local N = Mesh:AddNormal(Vector3.new(0, 0, -1))
	local Result = Mesh:BatchSetVertexFaceAttributes({ V[1], V[3] }, { F[1], F[3] }, { N, N })
	return Result, corners(Mesh, F[1]), corners(Mesh, F[3]), Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, { V[1], V[3] }, { F[1], F[3] }), counts(Mesh)
end)
try("set-uv-color", function()
	local Mesh, V, F = built()
	local U = Mesh:AddUV(Vector2.new(0.25, 0.75))
	local C = Mesh:AddColor(Color3.new(0, 1, 0), 0.5)
	Mesh:BatchSetVertexFaceAttributes({ V[2], V[2] }, { F[1], F[3] }, { U, C })
	return corners(Mesh, F[1]), corners(Mesh, F[3])
end)
try("set-then-triangle", function()
	local Mesh, V, F = built()
	local N = Mesh:AddNormal(Vector3.new(0, 0, -1))
	Mesh:BatchSetVertexFaceAttributes({ V[4] }, { F[2] }, { N })
	local G = Mesh:AddTriangle(V[4], V[5], V[6])
	return corners(Mesh, G)
end)
try("set-old-unused", function()
	local Mesh, V, F = built()
	local N = Mesh:AddNormal(Vector3.new(0, 0, -1))
	Mesh:BatchSetVertexFaceAttributes({ V[5] }, { F[3] }, { N })
	return counts(Mesh), Mesh:RemoveUnused(), counts(Mesh)
end)
try("set-fixed", function()
	local Mesh, V, F = built()
	local Copy = AssetService:CreateEditableMeshAsync(Content.fromObject(Mesh), { FixedSize = true })
	table.insert(Made, Copy)
	local N = Copy:GetNormals()[1]
	Copy:BatchSetVertexFaceAttributes({ V[2] }, { F[1] }, { N })
	return corners(Copy, F[1])
end)
try("set-twice-in-face", function()
	local Mesh, V = fresh()
	local F = Mesh:AddTriangle(V[1], V[1], V[2])
	local N = Mesh:AddNormal(Vector3.new(0, 0, -1))
	Mesh:BatchSetVertexFaceAttributes({ V[1] }, { F }, { N })
	return corners(Mesh, F)
end)
try("set-empty", function()
	local Mesh = built()
	return Mesh:BatchSetVertexFaceAttributes({}, {}, {})
end)
for _, Case in {
	{ "vertex-id", function(Mesh, V, F) return { V[1] }, { F[1] }, { V[6] } end },
	{ "face-id", function(Mesh, V, F) return { V[1] }, { F[1] }, { F[2] } end },
	{ "bone-id", function(Mesh, V, F) return { V[1] }, { F[1] }, { Mesh:AddBone({ Name = "b", CFrame = CFrame.new(), Virtual = false }) } end },
	{ "no-id", function(Mesh, V, F) return { V[1] }, { F[1] }, { 12345 } end },
	{ "gone-normal", function(Mesh, V, F) local N = Mesh:AddNormal(Vector3.xAxis) Mesh:RemoveUnused() return { V[1] }, { F[1] }, { N } end },
	{ "past-normal", function(Mesh, V, F, X) return { V[1] }, { F[1] }, { X.N + 100 } end },
	{ "not-in-face", function(Mesh, V, F, X) return { V[5] }, { F[1] }, { X.N } end },
	{ "short", function(Mesh, V, F, X) return { V[1], V[2] }, { F[1], F[1] }, { X.N } end },
	{ "short-faces", function(Mesh, V, F, X) return { V[1], V[2] }, { F[1] }, { X.N, X.N } end },
	{ "gone-face", function(Mesh, V, F, X) Mesh:RemoveFace(F[3]) return { V[2] }, { F[3] }, { X.N } end },
	{ "partial", function(Mesh, V, F, X) return { V[2], V[5] }, { F[1], F[1] }, { X.N, X.N } end },
	{ "word", function(Mesh, V, F) return { V[1] }, { F[1] }, { "x" } end },
} do
	try("set-" .. Case[1], function()
		local Mesh, V, F, X = built()
		local Vertices, Faces, Ids = Case[2](Mesh, V, F, X)
		local Ok, Problem = pcall(Mesh.BatchSetVertexFaceAttributes, Mesh, Vertices, Faces, Ids)
		return if Ok then "ok" else P.err(Problem), corners(Mesh, F[1]), counts(Mesh)
	end)
end
try("set-ids-nil", function()
	local Mesh, V, F = built()
	return Mesh:BatchSetVertexFaceAttributes({ V[1] }, { F[1] })
end)
try("set-single", function()
	local Mesh, V, F = built()
	local N = Mesh:AddNormal(Vector3.new(0, 0, -1))
	Mesh:SetVertexFaceNormal(V[2], F[1], N)
	return corners(Mesh, F[1])
end)

-- BatchRemove
try("remove-faces", function()
	local Mesh, V, F = built()
	local Result = Mesh:BatchRemove({ F[1], F[3] })
	return Result, counts(Mesh), Mesh:GetFaces(), Mesh:GetVertexFaces(V[2]), Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { V[2], V[5] })
end)
try("remove-twice", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[1] })
	local Ok, Problem = pcall(Mesh.BatchRemove, Mesh, { F[1] })
	return if Ok then "ok" else P.err(Problem), counts(Mesh)
end)
try("remove-same-twice", function()
	local Mesh, V, F = built()
	local Ok, Problem = pcall(Mesh.BatchRemove, Mesh, { F[1], F[1] })
	return if Ok then "ok" else P.err(Problem), counts(Mesh)
end)
try("remove-then-add", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[3] })
	return Mesh:AddTriangle(V[2], V[3], V[5]), Mesh:AddVertex(Vector3.one), counts(Mesh)
end)
try("remove-unused-after", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[3] })
	return counts(Mesh), Mesh:RemoveUnused(), counts(Mesh)
end)
for _, Case in {
	{ "vertex-free", function(Mesh, V) return { V[6] } end },
	{ "vertex-used", function(Mesh, V) return { V[5] } end },
	{ "vertex-shared", function(Mesh, V) return { V[1] } end },
	{ "normal-free", function(Mesh) return { Mesh:AddNormal(Vector3.xAxis) } end },
	{ "normal-used", function(Mesh, V, F, X) return { X.N } end },
	{ "uv-used", function(Mesh, V, F, X) return { X.U } end },
	{ "color-used", function(Mesh, V, F, X) return { X.C } end },
	{ "uv-free", function(Mesh) return { Mesh:AddUV(Vector2.one) } end },
	{ "color-free", function(Mesh) return { Mesh:AddColor(Color3.new(), 1) } end },
	{ "bone", function(Mesh) return { Mesh:AddBone({ Name = "b", CFrame = CFrame.new(), Virtual = false }) } end },
	{ "mixed", function(Mesh, V, F) return { F[3], V[6], Mesh:AddNormal(Vector3.xAxis) } end },
	{ "face-then-its-vertex", function(Mesh, V, F) return { F[3], V[5] } end },
	{ "no-id", function() return { 12345 } end },
	{ "zero", function() return { 0 } end },
	{ "past", function(Mesh, V, F) return { F[3] + 100 } end },
	{ "partial", function(Mesh, V, F) return { F[1], 12345 } end },
	{ "partial-gone", function(Mesh, V, F) Mesh:RemoveFace(F[3]) return { F[1], F[3] } end },
	{ "word", function() return { "x" } end },
	{ "hole", function(Mesh, V, F) return { F[1], nil, F[2] } end },
	{ "keyed", function(Mesh, V, F) return { a = F[1] } end },
	{ "empty", function() return {} end },
} do
	try("remove-" .. Case[1], function()
		local Mesh, V, F, X = built()
		local Ids = Case[2](Mesh, V, F, X)
		local Ok, Problem = pcall(Mesh.BatchRemove, Mesh, Ids)
		return if Ok then "ok" else P.err(Problem), counts(Mesh), Mesh:GetFaces(), corners(Mesh, F[2])
	end)
end
try("remove-fixed-face", function()
	local Mesh, V, F = built({ FixedSize = true })
	local Ok, Problem = pcall(Mesh.BatchRemove, Mesh, { F[1] })
	return if Ok then "ok" else P.err(Problem), counts(Mesh)
end)
try("remove-fixed-copy", function()
	local Mesh, V, F = built()
	local Copy = AssetService:CreateEditableMeshAsync(Content.fromObject(Mesh), { FixedSize = true })
	table.insert(Made, Copy)
	local Ok, Problem = pcall(Copy.BatchRemove, Copy, { F[1] })
	local Ok2, Problem2 = pcall(Copy.BatchRemove, Copy, { V[6] })
	local Ok3, Problem3 = pcall(Copy.RemoveFace, Copy, F[2])
	return if Ok then "ok" else P.err(Problem), if Ok2 then "ok" else P.err(Problem2), if Ok3 then "ok" else P.err(Problem3), counts(Copy)
end)
try("remove-not-table", function()
	local Mesh, V, F = built()
	return Mesh:BatchRemove(F[1])
end)
try("remove-nil", function()
	local Mesh = built()
	return Mesh:BatchRemove()
end)
-- the single-item removal beside it
try("remove-face-single-twice", function()
	local Mesh, V, F = built()
	Mesh:RemoveFace(F[1])
	local Ok, Problem = pcall(Mesh.RemoveFace, Mesh, F[1])
	return if Ok then "ok" else P.err(Problem), counts(Mesh)
end)
try("remove-face-single-vertex-id", function()
	local Mesh, V = built()
	return Mesh:RemoveFace(V[1])
end)

-- more of BatchGetVertexAttributes: a face moved onto a vertex, ids out of range, the arguments' checks
try("vertex-attributes-moved-face", function()
	local Mesh, V = fresh()
	local A = Mesh:AddTriangle(V[1], V[2], V[3])
	local B = Mesh:AddTriangle(V[1], V[3], V[4])
	local C = Mesh:AddTriangle(V[2], V[3], V[5])
	Mesh:SetFaceVertices(A, { V[1], V[5], V[3] })
	return A, B, C, Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Face, { V[1], V[2], V[3], V[5] }), Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { V[5] })
end)
for _, Case in {
	{ "index-zero", function() return { 4294967296 } end },
	{ "kind-seven", function() return { 7 * 4294967296 + 1 } end },
	{ "generation", function(Mesh, V) return { V[1] + 8 * 4294967296 } end },
	{ "negative", function() return { -1 } end },
	{ "huge", function() return { 2 ^ 60 } end },
	{ "nan", function() return { 0 / 0 } end },
	{ "boolean", function() return { true } end },
	{ "vector", function() return { Vector3.one } end },
} do
	try("vertex-attributes-" .. Case[1], function()
		local Mesh, V, F, X = built()
		return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, Case[2](Mesh, V, F, X))
	end)
end
try("vertex-attributes-ids-explicit-nil", function()
	local Mesh = built()
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, nil)
end)
try("vertex-attributes-extra", function()
	local Mesh, V = built()
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { V[1] }, {})
end)
try("vertex-attributes-none", function()
	local Mesh = built()
	return Mesh:BatchGetVertexAttributes()
end)
try("vertex-attributes-bad-attribute-no-ids", function()
	local Mesh = built()
	return Mesh:BatchGetVertexAttributes("x")
end)
try("vertex-attributes-bad-attribute-bad-ids", function()
	local Mesh = built()
	return Mesh:BatchGetVertexAttributes("x", 5)
end)
try("vertex-attributes-vertex-bad-ids", function()
	local Mesh = built()
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Vertex, { "x" })
end)
try("vertex-attributes-vertex-not-array", function()
	local Mesh = built()
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Vertex, 5)
end)
try("vertex-attributes-bad-then-word", function()
	local Mesh = built()
	return Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { 12345, "x" })
end)
try("batchadd-other-enum", function()
	local Mesh = fresh()
	return Mesh:BatchAdd(Enum.KeyCode.A, {})
end)

-- more of BatchGetVertexFaceAttributes and the single-item corner methods
try("vertex-face-twice-corners", function()
	local Mesh, V = fresh()
	local F = Mesh:AddTriangle(V[1], V[1], V[2])
	local N = { Mesh:AddNormal(Vector3.xAxis), Mesh:AddNormal(Vector3.yAxis), Mesh:AddNormal(Vector3.zAxis) }
	Mesh:SetFaceNormals(F, N)
	return N, Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, { V[1] }, { F }), Mesh:GetVertexFaceNormal(V[1], F), Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Normal, { V[1] })
end)
for _, Case in {
	{ "word-short", function(Mesh, V, F) return { "x" }, { F[1], F[2] } end },
	{ "bad-short", function(Mesh, V, F) return { V[1], 12345 }, { F[1] } end },
	{ "face-word", function(Mesh, V, F) return { V[1] }, { "x" } end },
	{ "face-word-second", function(Mesh, V, F) return { V[1], V[2] }, { F[1], "x" } end },
	{ "vertex-word-second", function(Mesh, V, F) return { V[1], "x" }, { F[1], F[1] } end },
	{ "not-in-face-then-bad", function(Mesh, V, F) return { V[5], 12345 }, { F[1], F[1] } end },
	{ "not-in-face-then-word", function(Mesh, V, F) return { V[5], "x" }, { F[1], F[1] } end },
	{ "bad-face-then-bad-vertex", function(Mesh, V, F) return { V[1], 12345 }, { 12345, F[1] } end },
	{ "zero-vertex", function(Mesh, V, F) return { 0 }, { F[1] } end },
	{ "zero-face", function(Mesh, V, F) return { V[1] }, { 0 } end },
	{ "string-ids", function(Mesh, V, F) return { tostring(V[1]) }, { tostring(F[1]) } end },
	{ "keyed", function(Mesh, V, F) return { a = V[1] }, { a = F[1] } end },
} do
	try("vertex-face-" .. Case[1], function()
		local Mesh, V, F, X = built()
		local Vertices, Faces = Case[2](Mesh, V, F, X)
		return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, Vertices, Faces)
	end)
end
try("vertex-face-vertex-bad-ids", function()
	local Mesh = built()
	return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Vertex, { "x" }, {})
end)
try("vertex-face-faces-explicit-nil", function()
	local Mesh, V = built()
	return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, { V[1] }, nil)
end)
try("vertex-face-faces-number", function()
	local Mesh, V = built()
	return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, { V[1] }, 5)
end)
for _, Case in {
	{ "single-uv-not-in-face", function(Mesh, V, F) return Mesh:GetVertexFaceUV(V[5], F[1]) end },
	{ "single-gone-face", function(Mesh, V, F) Mesh:RemoveFace(F[3]) return Mesh:GetVertexFaceNormal(V[2], F[3]) end },
	{ "single-swapped", function(Mesh, V, F) return Mesh:GetVertexFaceNormal(F[1], V[1]) end },
	{ "single-no-id-face", function(Mesh, V, F) return Mesh:GetVertexFaceColor(V[1], 12345) end },
	{ "single-zero", function(Mesh, V, F) return Mesh:GetVertexFaceNormal(0, F[1]) end },
	{ "single-word", function(Mesh, V, F) return Mesh:GetVertexFaceNormal("x", F[1]) end },
	{ "single-nil", function(Mesh, V, F) return Mesh:GetVertexFaceNormal(V[1]) end },
	{ "single-string", function(Mesh, V, F) return Mesh:GetVertexFaceNormal(tostring(V[1]), F[1]) end },
	{ "single-fraction", function(Mesh, V, F) return Mesh:GetVertexFaceNormal(V[1] + 0.5, F[1]) end },
	{ "single-set-uv", function(Mesh, V, F, X) Mesh:SetVertexFaceUV(V[1], F[1], X.U) return corners(Mesh, F[1]) end },
	{ "single-set-color", function(Mesh, V, F, X) Mesh:SetVertexFaceColor(V[3], F[3], X.C) return corners(Mesh, F[3]) end },
	{ "single-set-wrong-kind", function(Mesh, V, F, X) return Mesh:SetVertexFaceNormal(V[1], F[1], X.U) end },
	{ "single-set-not-in-face", function(Mesh, V, F, X) return Mesh:SetVertexFaceNormal(V[5], F[1], X.N) end },
	{ "single-set-gone-normal", function(Mesh, V, F) local N = Mesh:AddNormal(Vector3.xAxis) Mesh:RemoveUnused() return Mesh:SetVertexFaceNormal(V[1], F[1], N) end },
	{ "single-set-vertex-id", function(Mesh, V, F) return Mesh:SetVertexFaceNormal(V[1], F[1], V[2]) end },
	{ "single-set-no-id", function(Mesh, V, F) return Mesh:SetVertexFaceUV(V[1], F[1], 12345) end },
	{ "single-set-zero", function(Mesh, V, F) return Mesh:SetVertexFaceUV(V[1], F[1], 0) end },
	{ "single-set-returns", function(Mesh, V, F, X) return Mesh:SetVertexFaceNormal(V[1], F[1], X.N) end },
	{ "single-set-twice-in-face", function(Mesh, V, F, X) local G = Mesh:AddTriangle(V[6], V[6], V[5]) Mesh:SetVertexFaceNormal(V[6], G, X.N) return corners(Mesh, G) end },
} do
	try("vertex-face-" .. Case[1], function()
		local Mesh, V, F, X = built()
		return Case[2](Mesh, V, F, X)
	end)
end

-- more of BatchSetVertexFaceAttributes: what a failing item leaves of the ones before it
for _, Case in {
	{ "mixed-kinds", function(Mesh, V, F, X) return { V[2], V[2] }, { F[1], F[3] }, { X.U, X.C } end },
	{ "word-second", function(Mesh, V, F, X) return { V[2], V[2] }, { F[1], F[3] }, { X.N, "x" } end },
	{ "gone-second", function(Mesh, V, F, X) local N = Mesh:AddNormal(Vector3.xAxis) Mesh:RemoveUnused() return { V[2], V[2] }, { F[1], F[3] }, { X.N, N } end },
	{ "vertex-id-second", function(Mesh, V, F, X) return { V[2], V[2] }, { F[1], F[3] }, { X.N, V[1] } end },
	{ "vertex-word-second", function(Mesh, V, F, X) return { V[2], "x" }, { F[1], F[3] }, { X.N, X.N } end },
	{ "face-word-second", function(Mesh, V, F, X) return { V[2], V[2] }, { F[1], "x" }, { X.N, X.N } end },
	{ "bad-vertex-second", function(Mesh, V, F, X) return { V[2], 12345 }, { F[1], F[3] }, { X.N, X.N } end },
	{ "gone-face-second", function(Mesh, V, F, X) Mesh:RemoveFace(F[3]) return { V[2], V[2] }, { F[1], F[3] }, { X.N, X.N } end },
	{ "zero-second", function(Mesh, V, F, X) return { V[2], V[2] }, { F[1], F[3] }, { X.N, 0 } end },
	{ "repeat", function(Mesh, V, F, X) local M = Mesh:AddNormal(Vector3.yAxis) return { V[2], V[2] }, { F[1], F[1] }, { X.N, M } end },
	{ "generation", function(Mesh, V, F, X) return { V[2] }, { F[1] }, { X.N + 8 * 4294967296 } end },
	{ "zero-vertex", function(Mesh, V, F, X) return { 0 }, { F[1] }, { X.N } end },
	{ "string-ids", function(Mesh, V, F, X) return { tostring(V[2]) }, { tostring(F[1]) }, { tostring(X.N) } end },
} do
	try("set-" .. Case[1], function()
		local Mesh, V, F, X = built()
		local Vertices, Faces, Ids = Case[2](Mesh, V, F, X)
		local Ok, Problem = pcall(Mesh.BatchSetVertexFaceAttributes, Mesh, Vertices, Faces, Ids)
		return if Ok then "ok" else P.err(Problem), corners(Mesh, F[1]), counts(Mesh)
	end)
end
for _, Case in {
	{ "attributes-number", function(Mesh, V, F) return { V[1] }, { F[1] }, 5 end },
	{ "vertices-number", function(Mesh, V, F, X) return V[1], { F[1] }, { X.N } end },
	{ "faces-number", function(Mesh, V, F, X) return { V[1] }, F[1], { X.N } end },
	{ "attributes-explicit-nil", function(Mesh, V, F) return { V[1] }, { F[1] }, nil end },
} do
	try("set-" .. Case[1], function()
		local Mesh, V, F, X = built()
		local Vertices, Faces, Ids = Case[2](Mesh, V, F, X)
		return Mesh:BatchSetVertexFaceAttributes(Vertices, Faces, Ids)
	end)
end

-- more of BatchRemove: what it answers, ids it takes, and the ids a removed face's slot comes back under
try("remove-returns", function()
	local Mesh, V, F = built()
	return Mesh:BatchRemove({ F[1] })
end)
try("remove-string-id", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ tostring(F[1]), F[2] + 0.5 })
	return counts(Mesh)
end)
try("remove-generation", function()
	local Mesh, V, F = built()
	local Ok, Problem = pcall(Mesh.BatchRemove, Mesh, { F[1] + 8 * 4294967296 })
	return if Ok then "ok" else P.err(Problem), counts(Mesh)
end)
try("remove-index-zero", function()
	local Mesh = built()
	local Ok, Problem = pcall(Mesh.BatchRemove, Mesh, { 5 * 4294967296 })
	return if Ok then "ok" else P.err(Problem)
end)
try("remove-word-then-bad", function()
	local Mesh = built()
	local Ok, Problem = pcall(Mesh.BatchRemove, Mesh, { 12345, "x" })
	return if Ok then "ok" else P.err(Problem)
end)
try("remove-gone-then-bad", function()
	local Mesh, V, F = built()
	Mesh:RemoveFace(F[3])
	local Ok, Problem = pcall(Mesh.BatchRemove, Mesh, { F[3], 12345 })
	return if Ok then "ok" else P.err(Problem)
end)
try("remove-explicit-nil", function()
	local Mesh = built()
	return Mesh:BatchRemove(nil)
end)
try("remove-extra", function()
	local Mesh, V, F = built()
	return Mesh:BatchRemove({ F[1] }, {})
end)
try("reuse-one", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[2] })
	local G = Mesh:AddTriangle(V[1], V[3], V[4])
	return G, Mesh:GetFaces(), corners(Mesh, G), Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Face, { V[1], V[3] })
end)
try("reuse-order", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[1], F[3] })
	local G = { Mesh:AddTriangle(V[1], V[2], V[3]), Mesh:AddTriangle(V[2], V[3], V[5]), Mesh:AddTriangle(V[4], V[5], V[6]) }
	return G, Mesh:GetFaces()
end)
try("reuse-order-reversed", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[3], F[1] })
	local G = { Mesh:AddTriangle(V[1], V[2], V[3]), Mesh:AddTriangle(V[2], V[3], V[5]), Mesh:AddTriangle(V[4], V[5], V[6]) }
	return G, Mesh:GetFaces()
end)
try("reuse-singles", function()
	local Mesh, V, F = built()
	Mesh:RemoveFace(F[1])
	Mesh:RemoveFace(F[3])
	local G = { Mesh:AddTriangle(V[1], V[2], V[3]), Mesh:AddTriangle(V[2], V[3], V[5]), Mesh:AddTriangle(V[4], V[5], V[6]) }
	return G, Mesh:GetFaces()
end)
try("reuse-batchadd", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[1] })
	local G = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3] }, { V[2], V[3], V[5] }, { V[4], V[5], V[6] } })
	return G, Mesh:GetFaces()
end)
try("reuse-batchadd-two-free", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[1], F[3] })
	local G = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3] } })
	local H = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[2], V[3], V[5] }, { V[4], V[5], V[6] } })
	return G, H, Mesh:GetFaces()
end)
try("reuse-twice", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[3] })
	local G = Mesh:AddTriangle(V[2], V[3], V[5])
	Mesh:BatchRemove({ G })
	local H = Mesh:AddTriangle(V[2], V[3], V[5])
	local Ok, Problem = pcall(Mesh.BatchRemove, Mesh, { F[3] })
	return G, H, if Ok then "ok" else P.err(Problem), Mesh:GetFaces()
end)
try("reuse-old-id", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[3] })
	local G = Mesh:AddTriangle(V[2], V[3], V[5])
	local Ok, Problem = pcall(Mesh.GetFaceVertices, Mesh, F[3])
	local Ok2, Problem2 = pcall(Mesh.BatchGetVertexFaceAttributes, Mesh, Enum.MeshAttribute.Normal, { V[2] }, { F[3] })
	return G, if Ok then show(Problem) else P.err(Problem), if Ok2 then show(Problem2) else P.err(Problem2)
end)
try("reuse-vertices", function()
	local Mesh = built()
	local Removed = Mesh:RemoveUnused()
	return Removed, Mesh:AddVertex(Vector3.one), Mesh:AddVertex(Vector3.one), Mesh:GetVertices()
end)
try("reuse-vertices-batch", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[3] })
	local Removed = Mesh:RemoveUnused()
	return Removed, Mesh:BatchAdd(Enum.MeshAttribute.Vertex, { Vector3.one, Vector3.one, Vector3.one }), Mesh:GetVertices()
end)
try("reuse-normals", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[3] })
	local Removed = Mesh:RemoveUnused()
	return Removed, Mesh:AddNormal(Vector3.xAxis), Mesh:AddUV(Vector2.one), Mesh:AddColor(Color3.new(), 1), Mesh:GetNormals()
end)
try("reuse-triangle-attributes", function()
	local Mesh, V, F = built()
	Mesh:BatchRemove({ F[3] })
	Mesh:RemoveUnused()
	local W = Mesh:AddVertex(Vector3.one)
	local G = Mesh:AddTriangle(W, V[2], V[4])
	return W, G, corners(Mesh, G)
end)
try("remove-all-then-unused", function()
	local Mesh = built()
	Mesh:BatchRemove(Mesh:GetFaces())
	return counts(Mesh), Mesh:RemoveUnused(), counts(Mesh)
end)

-- fixed size: the option and the add / remove errors
try("fixed-option", function()
	local Mesh = AssetService:CreateEditableMesh({ FixedSize = true })
	table.insert(Made, Mesh)
	local Copy = AssetService:CreateEditableMeshAsync(Content.fromObject(Mesh), { FixedSize = false })
	table.insert(Made, Copy)
	local Plain = AssetService:CreateEditableMeshAsync(Content.fromObject(Mesh))
	table.insert(Made, Plain)
	return Mesh.FixedSize, Copy.FixedSize, Plain.FixedSize
end)
for _, Case in {
	{ "add-vertex", function(Copy) return Copy:AddVertex(Vector3.one) end },
	{ "add-triangle", function(Copy, V) return Copy:AddTriangle(V[4], V[5], V[6]) end },
	{ "batchadd-vertex", function(Copy) return Copy:BatchAdd(Enum.MeshAttribute.Vertex, { Vector3.one }) end },
	{ "batchadd-face", function(Copy, V) return Copy:BatchAdd(Enum.MeshAttribute.Face, { { V[4], V[5], V[6] } }) end },
	{ "add-normal", function(Copy) return Copy:AddNormal(Vector3.one) end },
	{ "add-uv", function(Copy) return Copy:AddUV(Vector2.one) end },
	{ "add-color", function(Copy) return Copy:AddColor(Color3.new(), 1) end },
	{ "remove-unused", function(Copy) return Copy:RemoveUnused() end },
	{ "remove-empty", function(Copy) return Copy:BatchRemove({}) end },
	{ "set-face-vertices", function(Copy, V, F) Copy:SetFaceVertices(F[1], { V[4], V[5], V[6] }) return corners(Copy, F[1]) end },
} do
	try("fixed-" .. Case[1], function()
		local Mesh, V, F = built()
		local Copy = AssetService:CreateEditableMeshAsync(Content.fromObject(Mesh), { FixedSize = true })
		table.insert(Made, Copy)
		local Results = table.pack(pcall(Case[2], Copy, V, F))
		return if Results[1] then show(Results[2]) else P.err(Results[2]), counts(Copy)
	end)
end

-- which edits bring a face's corner to the front of its vertex's faces: A (v1 v2 v3) then B (v1 v3 v4) list v1's
-- faces B, A; each edit below is to A, then v1's faces and the normal a new face over v1 takes
for _, Case in {
	{ "normals", function(Mesh, V, A, X) local O = Mesh:GetFaceNormals(A) Mesh:SetFaceNormals(A, { X.N, O[2], O[3] }) end },
	{ "normals-same", function(Mesh, V, A) Mesh:SetFaceNormals(A, Mesh:GetFaceNormals(A)) end },
	{ "normals-other-corner", function(Mesh, V, A, X) local O = Mesh:GetFaceNormals(A) Mesh:SetFaceNormals(A, { O[1], X.N, O[3] }) end },
	{ "uvs", function(Mesh, V, A, X) local O = Mesh:GetFaceUVs(A) Mesh:SetFaceUVs(A, { X.U, O[2], O[3] }) end },
	{ "colors", function(Mesh, V, A, X) local O = Mesh:GetFaceColors(A) Mesh:SetFaceColors(A, { X.C, O[2], O[3] }) end },
	{ "vertices-same", function(Mesh, V, A) Mesh:SetFaceVertices(A, Mesh:GetFaceVertices(A)) end },
	{ "vertex-face-normal", function(Mesh, V, A, X) Mesh:SetVertexFaceNormal(V[1], A, X.N) end },
	{ "vertex-face-normal-same", function(Mesh, V, A) Mesh:SetVertexFaceNormal(V[1], A, Mesh:GetVertexFaceNormal(V[1], A)) end },
	{ "vertex-face-uv", function(Mesh, V, A, X) Mesh:SetVertexFaceUV(V[1], A, X.U) end },
	{ "batch-vertex-face", function(Mesh, V, A, X) Mesh:BatchSetVertexFaceAttributes({ V[1] }, { A }, { X.N }) end },
	{ "batch-vertex-face-same", function(Mesh, V, A) Mesh:BatchSetVertexFaceAttributes({ V[1] }, { A }, { Mesh:GetVertexFaceNormal(V[1], A) }) end },
	{ "batch-face", function(Mesh, V, A, X) local O = Mesh:GetFaceNormals(A) Mesh:BatchSetFaceAttributes({ A }, { { X.N, O[2], O[3] } }) end },
	{ "set-normal-value", function(Mesh, V, A) Mesh:SetNormal(Mesh:GetFaceNormals(A)[1], Vector3.zAxis) end },
	{ "remove-other", function(Mesh, V, A, X, B) Mesh:BatchRemove({ B }) end },
} do
	try("touch-" .. Case[1], function()
		local Mesh, V = fresh()
		local A = Mesh:AddTriangle(V[1], V[2], V[3])
		local B = Mesh:AddTriangle(V[1], V[3], V[4])
		local X = { N = Mesh:AddNormal(Vector3.xAxis), U = Mesh:AddUV(Vector2.one), C = Mesh:AddColor(Color3.new(1, 0, 0), 1) }
		Case[2](Mesh, V, A, X, B)
		local Faces = Mesh:BatchGetVertexAttributes(Enum.MeshAttribute.Face, { V[1], V[2], V[3] })
		local G = Mesh:AddTriangle(V[1], V[5], V[6])
		return Faces, corners(Mesh, G)
	end)
end

-- the order BatchRemove frees faces in (their ids, or their places in GetFaces), and the slots that come back
try("remove-order-batchadd", function()
	local Mesh, V = fresh()
	local F = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3] }, { V[1], V[3], V[4] }, { V[2], V[3], V[5] }, { V[4], V[5], V[6] } })
	Mesh:BatchRemove({ F[1], F[3] })
	local Left = Mesh:GetFaces()
	local G = { Mesh:AddTriangle(V[1], V[2], V[3]), Mesh:AddTriangle(V[2], V[3], V[5]), Mesh:AddTriangle(V[4], V[5], V[6]) }
	return F, Left, G, Mesh:GetFaces()
end)
try("remove-order-moved", function()
	local Mesh, V, F = built()
	Mesh:RemoveFace(F[1])
	local G = Mesh:AddTriangle(V[1], V[2], V[3])
	local Before = Mesh:GetFaces()
	Mesh:BatchRemove({ F[3], G })
	local Left = Mesh:GetFaces()
	return G, Before, Left, Mesh:AddTriangle(V[1], V[2], V[3]), Mesh:AddTriangle(V[2], V[3], V[5]), Mesh:GetFaces()
end)
-- GetVertices / GetNormals after a removal from the middle, and the order RemoveUnused goes in
try("unused-middle", function()
	local Mesh, V = fresh()
	Mesh:AddTriangle(V[1], V[3], V[4])
	Mesh:AddTriangle(V[4], V[5], V[6])
	local Removed = Mesh:RemoveUnused()
	local Vertices = Mesh:GetVertices()
	local W = Mesh:AddVertex(Vector3.one)
	return Removed, Vertices, W, Mesh:GetVertices()
end)
try("unused-order", function()
	local Mesh, V = fresh()
	local A = Mesh:AddTriangle(V[1], V[3], V[4])
	local B = Mesh:AddTriangle(V[4], V[5], V[6])
	Mesh:RemoveUnused()
	local W = Mesh:AddVertex(Vector3.one)
	local C = Mesh:AddTriangle(W, V[1], V[3])
	local Normals = Mesh:GetNormals()
	Mesh:BatchRemove({ B, C })
	return Normals, Mesh:GetVertices(), Mesh:RemoveUnused(), Mesh:GetVertices(), Mesh:GetNormals()
end)
try("unused-middle-normals", function()
	local Mesh, V = fresh()
	local A = Mesh:AddTriangle(V[1], V[2], V[3])
	local B = Mesh:AddTriangle(V[4], V[5], V[6])
	local O = Mesh:GetFaceNormals(A)
	Mesh:SetFaceNormals(A, { O[1], O[1], O[1] })
	local Removed = Mesh:RemoveUnused()
	local Normals = Mesh:GetNormals()
	return Removed, Normals, Mesh:AddNormal(Vector3.one), Mesh:GetNormals()
end)
-- an int64 argument's rounding, the fixed-size check against the others, AddBone's fields
try("single-round", function()
	local Mesh, V, F = built()
	return Mesh:GetVertexFaceNormal(V[2] + 0.5, F[1]), Mesh:GetVertexFaceNormal(V[2] + 0.25, F[1]), Mesh:GetVertexFaceNormal(V[3] - 0.5, F[1])
end)
try("single-set-check-order", function()
	local Mesh, V, F = built()
	return Mesh:SetVertexFaceNormal(V[5], F[1], V[2])
end)
for _, Case in {
	{ "not-in-face-bad-kind", function(Mesh, V, F) return { V[5] }, { F[1] }, { V[6] } end },
	{ "zero-first", function(Mesh, V, F) return { V[1] }, { F[1] }, { 0 } end },
	{ "both-short", function(Mesh, V, F, X) return { V[1], V[2] }, { F[1] }, { X.N } end },
	{ "bad-vertex-bad-kind", function(Mesh, V, F) return { 12345 }, { F[1] }, { V[6] } end },
	{ "bad-face-bad-kind", function(Mesh, V, F) return { V[1] }, { 12345 }, { V[6] } end },
} do
	try("set-" .. Case[1], function()
		local Mesh, V, F, X = built()
		local Vertices, Faces, Ids = Case[2](Mesh, V, F, X)
		return Mesh:BatchSetVertexFaceAttributes(Vertices, Faces, Ids)
	end)
end
for _, Case in {
	{ "batchadd-empty", function(Copy) return Copy:BatchAdd(Enum.MeshAttribute.Vertex, {}) end },
	{ "batchadd-not-array", function(Copy) return Copy:BatchAdd(Enum.MeshAttribute.Vertex, 5) end },
	{ "batchadd-faces-empty", function(Copy) return Copy:BatchAdd(Enum.MeshAttribute.Face, {}) end },
	{ "remove-not-array", function(Copy) return Copy:BatchRemove(5) end },
	{ "remove-bad", function(Copy) return Copy:BatchRemove({ 12345 }) end },
	{ "remove-face-bad", function(Copy) return Copy:RemoveFace(12345) end },
	{ "add-triangle-bad", function(Copy) return Copy:AddTriangle(12345, 12345, 12345) end },
	{ "add-bone", function(Copy) return Copy:AddBone({ Name = "b", CFrame = CFrame.new(), Virtual = false }) end },
	{ "remove-bone", function(Copy) return Copy:RemoveBone(12345) end },
} do
	try("fixed-" .. Case[1], function()
		local Mesh, V, F = built()
		local Copy = AssetService:CreateEditableMeshAsync(Content.fromObject(Mesh))
		table.insert(Made, Copy)
		local Results = table.pack(pcall(Case[2], Copy, V, F))
		return if Results[1] then show(Results[2]) else P.err(Results[2]), counts(Copy)
	end)
end
for _, Case in {
	{ "empty", {} },
	{ "no-name", { CFrame = CFrame.new(), Virtual = false } },
	{ "no-virtual", { Name = "b", CFrame = CFrame.new() } },
	{ "no-cframe", { Name = "b", Virtual = false } },
	{ "name-number", { Name = 5, CFrame = CFrame.new(), Virtual = false } },
	{ "full", { Name = "b", CFrame = CFrame.new(1, 2, 3), Virtual = true } },
} do
	try("bone-" .. Case[1], function()
		local Mesh = fresh()
		local B = Mesh:AddBone(Case[2])
		return B, Mesh:GetBoneName(B), Mesh:GetBoneIsVirtual(B)
	end)
end

-- the checks' order within one item, and the single-item methods' own errors
try("vertex-face-bad-vertex-face-word", function()
	local Mesh = built()
	return Mesh:BatchGetVertexFaceAttributes(Enum.MeshAttribute.Normal, { 12345 }, { "x" })
end)
try("set-bad-vertex-face-word", function()
	local Mesh, V, F, X = built()
	return Mesh:BatchSetVertexFaceAttributes({ 12345 }, { "x" }, { X.N })
end)
try("set-not-in-face-attribute-word", function()
	local Mesh, V, F, X = built()
	return Mesh:BatchSetVertexFaceAttributes({ V[1], V[5] }, { F[1], F[1] }, { X.N, "x" })
end)
for _, Case in {
	{ "remove-face-zero", function(Mesh) return Mesh:RemoveFace(0) end },
	{ "remove-face-word", function(Mesh) return Mesh:RemoveFace("x") end },
	{ "vertex-face-boolean", function(Mesh, V, F) return Mesh:GetVertexFaceNormal(true, F[1]) end },
	{ "vertex-face-zero-face", function(Mesh, V) return Mesh:GetVertexFaceNormal(V[1], 0) end },
	{ "vertex-faces-gone", function(Mesh, V) Mesh:RemoveUnused() return Mesh:GetVertexFaces(V[6]) end },
	{ "vertex-normals-no-id", function(Mesh) return Mesh:GetVertexNormals(12345) end },
	{ "vertex-uvs-face-id", function(Mesh, V, F) return Mesh:GetVertexUVs(F[1]) end },
	{ "vertex-colors-zero", function(Mesh) return Mesh:GetVertexColors(0) end },
	{ "adjacent-gone", function(Mesh, V) Mesh:RemoveUnused() return Mesh:GetAdjacentVertices(V[6]) end },
} do
	try("single-" .. Case[1], function()
		local Mesh, V, F, X = built()
		return Case[2](Mesh, V, F, X)
	end)
end
