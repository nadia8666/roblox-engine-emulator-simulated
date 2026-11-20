-- EditableMesh:BatchAdd, which normal / uv / colour a new face's corner takes, and the value of an automatic normal
-- several faces share (a playing client).
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
		for _, Item in Value do
			table.insert(Parts, show(Item))
		end
		return "{" .. table.concat(Parts, ",") .. "}"
	end
	return Kind .. ":" .. tostring(Value)
end
local Current
local function try(Label, Function)
	local Results = table.pack(pcall(Function))
	if Current then
		Current:Destroy()
		Current = nil
	end
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
local function fresh(Positions)
	local Mesh = AssetService:CreateEditableMesh()
	Current = Mesh
	local V = {}
	for _, Position in Positions or { Vector3.new(0, 0, 0), Vector3.new(1, 0, 0), Vector3.new(1, 1, 0), Vector3.new(0, 1, 0), Vector3.new(0, 0, 1), Vector3.new(2, 2, 2) } do
		table.insert(V, Mesh:AddVertex(Position))
	end
	return Mesh, V
end
local function counts(Mesh)
	return #Mesh:GetVertices() .. "v " .. #Mesh:GetNormals() .. "n " .. #Mesh:GetUVs() .. "u " .. #Mesh:GetColors() .. "c " .. #Mesh:GetFaces() .. "f"
end
local function corners(Mesh, Face)
	return "n" .. show(Mesh:GetFaceNormals(Face)) .. " u" .. show(Mesh:GetFaceUVs(Face)) .. " c" .. show(Mesh:GetFaceColors(Face))
end

-- BatchAdd of each attribute
try("vertices", function()
	local Mesh = fresh({})
	local Ids = Mesh:BatchAdd(Enum.MeshAttribute.Vertex, { Vector3.new(0, 0, 0), Vector3.new(1, 0, 0), Vector3.new(0, 1, 0) })
	return Ids, #Mesh:GetVertices(), Mesh:GetPosition(Ids[2])
end)
try("between", function()
	local Mesh = fresh({})
	return Mesh:AddVertex(Vector3.zero), Mesh:BatchAdd(Enum.MeshAttribute.Vertex, { Vector3.xAxis, Vector3.yAxis }), Mesh:AddVertex(Vector3.zAxis)
end)
try("normals", function()
	local Mesh = fresh({})
	local Ids = Mesh:BatchAdd(Enum.MeshAttribute.Normal, { Vector3.new(0, 0, 1), nil, Vector3.new(0, 1, 0) })
	return Ids, Mesh:GetNormal(Ids[1]), Mesh:GetNormal(Ids[3])
end)
try("uvs", function()
	local Mesh = fresh({})
	local Ids = Mesh:BatchAdd(Enum.MeshAttribute.UV, { Vector2.new(0.5, 0.25) })
	return Ids, Mesh:GetUV(Ids[1])
end)
try("colors", function()
	local Mesh = fresh({})
	local Ids = Mesh:BatchAdd(Enum.MeshAttribute.Color, { Color3.new(1, 0, 0), Color3.new(0, 0.5, 1), Color3.new() }, { 0.5, 2, -1 })
	return Ids, Mesh:GetColor(Ids[1]), Mesh:GetColorAlpha(Ids[1]), Mesh:GetColorAlpha(Ids[2]), Mesh:GetColorAlpha(Ids[3])
end)
try("faces", function()
	local Mesh, V = fresh()
	local Ids = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3] }, { V[1], V[3], V[4] }, { V[2], V[3], V[5] } })
	return Ids, counts(Mesh), corners(Mesh, Ids[1]), corners(Mesh, Ids[2]), corners(Mesh, Ids[3]), Mesh:GetFaces()
end)
try("faces-then-triangle", function()
	local Mesh, V = fresh()
	local Ids = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3] }, { V[1], V[3], V[4] }, { V[2], V[3], V[5] } })
	local T = Mesh:AddTriangle(V[4], V[5], V[6])
	local More = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[4], V[6] }, { V[2], V[4], V[6] } })
	return Ids, T, More, Mesh:GetFaces(), counts(Mesh)
end)
try("face-order", function()
	local Mesh, V = fresh()
	local Ids = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[3], V[2], V[1] }, { V[4], V[2], V[5] } })
	return Ids, corners(Mesh, Ids[1]), corners(Mesh, Ids[2])
end)
try("face-same-vertex-twice", function()
	local Mesh, V = fresh()
	local Ids = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[1], V[2] } })
	return Ids, counts(Mesh), corners(Mesh, Ids[1])
end)
try("corner-values", function()
	local Mesh, V = fresh()
	local Ids = Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3] } })
	local C = Mesh:GetFaceColors(Ids[1])
	return Mesh:GetNormal(Mesh:GetFaceNormals(Ids[1])[1]), Mesh:GetUV(Mesh:GetFaceUVs(Ids[1])[1]), Mesh:GetColor(C[1]), Mesh:GetColorAlpha(C[1])
end)
try("empty", function()
	local Mesh = fresh({})
	return Mesh:BatchAdd(Enum.MeshAttribute.Vertex, {}), Mesh:BatchAdd(Enum.MeshAttribute.Vertex, { a = Vector3.one })
end)
-- partly made before an error
try("partial-vertices", function()
	local Mesh = fresh({})
	local Problem = select(2, pcall(function()
		Mesh:BatchAdd(Enum.MeshAttribute.Vertex, { Vector3.new(1, 1, 1), "x", Vector3.new(3, 3, 3) })
	end))
	local Out = {}
	for _, Id in Mesh:GetVertices() do
		table.insert(Out, show(Mesh:GetPosition(Id)))
	end
	return P.err(Problem), table.concat(Out, " ")
end)
try("partial-normals", function()
	local Mesh = fresh({})
	local Problem = select(2, pcall(function()
		Mesh:BatchAdd(Enum.MeshAttribute.Normal, { Vector3.new(1, 0, 0), 5 })
	end))
	return P.err(Problem), counts(Mesh)
end)
try("partial-colors", function()
	local Mesh = fresh({})
	local Problem = select(2, pcall(function()
		Mesh:BatchAdd(Enum.MeshAttribute.Color, { Color3.new(), Vector3.one }, { 1, 1 })
	end))
	return P.err(Problem), counts(Mesh)
end)
try("partial-faces", function()
	local Mesh, V = fresh()
	local Problem = select(2, pcall(function()
		Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3] }, { V[1], V[2] } })
	end))
	return P.err(Problem), counts(Mesh)
end)
-- errors
for _, Case in {
	{ "vertex-wrong", Enum.MeshAttribute.Vertex, { Vector3.one, "x" } },
	{ "uv-wrong", Enum.MeshAttribute.UV, { Vector2.zero, 5 } },
	{ "color-wrong", Enum.MeshAttribute.Color, { Color3.new(), 5 }, { 1, 1 } },
	{ "color-short-alphas", Enum.MeshAttribute.Color, { Color3.new(), Color3.new() }, { 1 } },
	{ "color-no-alphas", Enum.MeshAttribute.Color, {} },
	{ "vertex-extra", Enum.MeshAttribute.Vertex, {}, {} },
	{ "normal-extra", Enum.MeshAttribute.Normal, {}, {} },
	{ "uv-extra", Enum.MeshAttribute.UV, {}, {} },
	{ "face-extra", Enum.MeshAttribute.Face, {}, {} },
	{ "vertex-not-array", Enum.MeshAttribute.Vertex, 5 },
	{ "normal-not-array", Enum.MeshAttribute.Normal, 5 },
	{ "uv-not-array", Enum.MeshAttribute.UV, 5 },
	{ "colors-not-array", Enum.MeshAttribute.Color, 5, {} },
	{ "alphas-not-array", Enum.MeshAttribute.Color, {}, 5 },
	{ "faces-not-array", Enum.MeshAttribute.Face, 5 },
	{ "attribute-string", "Vertex", {} },
	{ "attribute-number", 0, {} },
	{ "attribute-nil", nil, {} },
} do
	try(Case[1], function()
		local Mesh = fresh({})
		if Case[4] ~= nil then
			return Mesh:BatchAdd(Case[2], Case[3], Case[4])
		end
		return Mesh:BatchAdd(Case[2], Case[3])
	end)
end
try("no-data", function()
	local Mesh = fresh({})
	return Mesh:BatchAdd(Enum.MeshAttribute.Vertex)
end)
try("face-not-array", function()
	local Mesh, V = fresh()
	return Mesh:BatchAdd(Enum.MeshAttribute.Face, { V[1] })
end)
try("face-five", function()
	local Mesh, V = fresh()
	return Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3], V[4], V[5] } })
end)
try("face-normal-id", function()
	local Mesh, V = fresh()
	local N = Mesh:AddNormal(Vector3.new(0, 0, 1))
	return Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], N } })
end)
try("face-no-id", function()
	local Mesh, V = fresh()
	return Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], 12345 } })
end)
try("face-gone-vertex", function()
	local Mesh, V = fresh()
	Mesh:RemoveUnused()
	return Mesh:BatchAdd(Enum.MeshAttribute.Face, { { V[1], V[2], V[3] } })
end)

-- AddTriangle's corners
try("share-edge", function()
	local Mesh, V = fresh()
	local A = Mesh:AddTriangle(V[1], V[2], V[3])
	local B = Mesh:AddTriangle(V[1], V[3], V[5])
	local N = Mesh:GetFaceNormals(B)
	return counts(Mesh), corners(Mesh, A), corners(Mesh, B), Mesh:GetNormal(N[1]), Mesh:GetNormal(N[2]), Mesh:GetNormal(N[3])
end)
try("set-normal", function()
	local Mesh, V = fresh()
	local A = Mesh:AddTriangle(V[1], V[2], V[3])
	local N = Mesh:AddNormal(Vector3.new(1, 0, 0))
	local Old = Mesh:GetFaceNormals(A)
	Mesh:SetFaceNormals(A, { N, Old[2], Old[3] })
	local B = Mesh:AddTriangle(V[1], V[3], V[4])
	return counts(Mesh), corners(Mesh, A), corners(Mesh, B)
end)
try("newest-face", function()
	local Mesh, V = fresh()
	local A = Mesh:AddTriangle(V[1], V[2], V[3])
	local B = Mesh:AddTriangle(V[1], V[3], V[4])
	local NA = Mesh:AddNormal(Vector3.new(0, 1, 0))
	local Old = Mesh:GetFaceNormals(B)
	Mesh:SetFaceNormals(B, { NA, Old[2], Old[3] })
	local C = Mesh:AddTriangle(V[1], V[4], V[5])
	return counts(Mesh), corners(Mesh, C)
end)
try("set-uv-color", function()
	local Mesh, V = fresh()
	local A = Mesh:AddTriangle(V[1], V[2], V[3])
	local U = Mesh:AddUV(Vector2.new(0.5, 0.5))
	local C = Mesh:AddColor(Color3.new(1, 0, 0), 1)
	local OldU = Mesh:GetFaceUVs(A)
	local OldC = Mesh:GetFaceColors(A)
	Mesh:SetFaceUVs(A, { U, OldU[2], OldU[3] })
	Mesh:SetFaceColors(A, { C, OldC[2], OldC[3] })
	local B = Mesh:AddTriangle(V[1], V[3], V[4])
	return counts(Mesh), corners(Mesh, B)
end)
try("removed-face", function()
	local Mesh, V = fresh()
	Mesh:RemoveFace(Mesh:AddTriangle(V[1], V[2], V[3]))
	local B = Mesh:AddTriangle(V[1], V[2], V[3])
	return counts(Mesh), corners(Mesh, B)
end)

-- shared automatic normals
for _, Case in {
	{ "fold", { Vector3.new(0, 0, 0), Vector3.new(1, 0, 0), Vector3.new(1, 1, 0), Vector3.new(0, 0, 1) }, { { 1, 2, 3 }, { 1, 3, 4 } } },
	{ "box-corner", { Vector3.new(0, 0, 0), Vector3.new(2, 0, 0), Vector3.new(0, 1, 0), Vector3.new(0, 0, 3) }, { { 1, 2, 3 }, { 1, 3, 4 }, { 1, 4, 2 } } },
	{ "fan", { Vector3.new(0, 0, 0), Vector3.new(1, 0, 0), Vector3.new(0.3, 1, 0.2), Vector3.new(-1, 0.5, 0.6), Vector3.new(-0.2, -1, 0.1) }, { { 1, 2, 3 }, { 1, 3, 4 }, { 1, 4, 5 }, { 1, 5, 2 } } },
	{ "thin", { Vector3.new(0, 0, 0), Vector3.new(10, 0, 0), Vector3.new(10, 0.1, 0), Vector3.new(0, 0, 1) }, { { 1, 2, 3 }, { 1, 3, 4 } } },
	{ "twice", { Vector3.new(0, 0, 0), Vector3.new(1, 0, 0), Vector3.new(0, 1, 0), Vector3.new(0, 0, 1) }, { { 1, 2, 3 }, { 1, 2, 3 }, { 1, 3, 4 } } },
} do
	try("normals-" .. Case[1], function()
		local Mesh, V = fresh(Case[2])
		local Faces = Mesh:BatchAdd(Enum.MeshAttribute.Face, (function()
			local Out = {}
			for _, F in Case[3] do
				table.insert(Out, { V[F[1]], V[F[2]], V[F[3]] })
			end
			return Out
		end)())
		local Seen, Out = {}, {}
		for _, Face in Mesh:GetFaces() do
			local Normals, Vertices = Mesh:GetFaceNormals(Face), Mesh:GetFaceVertices(Face)
			for Corner = 1, 3 do
				if not Seen[Normals[Corner]] then
					Seen[Normals[Corner]] = true
					table.insert(Out, "v" .. table.find(V, Vertices[Corner]) .. "=" .. show(Mesh:GetNormal(Normals[Corner])))
				end
			end
		end
		return table.concat(Out, " ")
	end)
end
