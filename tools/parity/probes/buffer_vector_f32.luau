-- buffer.readvectorf32x2/x3 and buffer.writevectorf32x2/x3 (Roblox release 747) against the float32 calls their design
-- (luau-lang/rfcs pull 198) defines them by. An engine that has the functions runs them; one that does not runs the
-- reference below, so the lines say what the functions must print either way. The argument-error lines name the
-- function and the vector type, which the reference cannot say: they are checked only where the functions exist.
local Reference = {
	readvectorf32x2 = function(Target, Offset)
		return vector.create(buffer.readf32(Target, Offset), buffer.readf32(Target, Offset + 4), 0)
	end,
	readvectorf32x3 = function(Target, Offset)
		return vector.create(buffer.readf32(Target, Offset), buffer.readf32(Target, Offset + 4), buffer.readf32(Target, Offset + 8))
	end,
	writevectorf32x2 = function(Target, Offset, Value)
		-- (the bounds of the whole write come first: nothing is written when the last component does not fit)
		buffer.readf32(Target, Offset + 4)
		buffer.writef32(Target, Offset, Value.X)
		buffer.writef32(Target, Offset + 4, Value.Y)
	end,
	writevectorf32x3 = function(Target, Offset, Value)
		buffer.readf32(Target, Offset + 8)
		buffer.writef32(Target, Offset, Value.X)
		buffer.writef32(Target, Offset + 4, Value.Y)
		buffer.writef32(Target, Offset + 8, Value.Z)
	end,
}
local Native = buffer.readvectorf32x3 ~= nil
local Lib = if Native then buffer else Reference
print("vector-f32-native " .. tostring(Native))

local function Bytes(Target)
	local Out = {}
	for Index = 0, buffer.len(Target) - 1 do
		table.insert(Out, string.format("%02X", buffer.readu8(Target, Index)))
	end
	return table.concat(Out)
end

local function Components(Value)
	return P.f32s(Value.X, Value.Y, Value.Z)
end

-- an outcome as one line; an error keeps its text without the name of the function that raised it, and without the
-- line the reference's own Luau adds
local function Try(Function, ...)
	local Results = table.pack(pcall(Function, ...))
	if not Results[1] then
		return "error " .. (string.gsub(string.gsub(P.err(Results[2]), "^@%d+: ", ""), "'[%w_]+'", "'F'"))
	end
	local Parts = { "ok", tostring(Results.n - 1) }
	for Index = 2, Results.n do
		table.insert(Parts, if type(Results[Index]) == "vector" then Components(Results[Index]) else tostring(Results[Index]))
	end
	return table.concat(Parts, " ")
end

local function Floats(...)
	local Values = table.pack(...)
	local Target = buffer.create(Values.n * 4)
	for Index = 1, Values.n do
		buffer.writef32(Target, (Index - 1) * 4, Values[Index])
	end
	return Target
end

-- reads
local Source = Floats(1.5, -2.25, 1 / 3, 1e30, -0, 7)
print("read-x3 " .. Try(Lib.readvectorf32x3, Source, 0) .. " | " .. Try(Lib.readvectorf32x3, Source, 4) .. " | " .. Try(Lib.readvectorf32x3, Source, 12))
print("read-x2 " .. Try(Lib.readvectorf32x2, Source, 0) .. " | " .. Try(Lib.readvectorf32x2, Source, 8) .. " | " .. Try(Lib.readvectorf32x2, Source, 16))
print("read-unaligned " .. Try(Lib.readvectorf32x3, Source, 1) .. " | " .. Try(Lib.readvectorf32x2, Source, 3))
print("read-x3-bounds last " .. Try(Lib.readvectorf32x3, Source, 12) .. " | past " .. Try(Lib.readvectorf32x3, Source, 13) .. " | end " .. Try(Lib.readvectorf32x3, Source, 24) .. " | negative " .. Try(Lib.readvectorf32x3, Source, -1))
print("read-x2-bounds last " .. Try(Lib.readvectorf32x2, Source, 16) .. " | past " .. Try(Lib.readvectorf32x2, Source, 17) .. " | end " .. Try(Lib.readvectorf32x2, Source, 24) .. " | negative " .. Try(Lib.readvectorf32x2, Source, -4))
print("read-short x3 " .. Try(Lib.readvectorf32x3, buffer.create(11), 0) .. " | " .. Try(Lib.readvectorf32x3, buffer.create(12), 0) .. " | x2 " .. Try(Lib.readvectorf32x2, buffer.create(7), 0) .. " | " .. Try(Lib.readvectorf32x2, buffer.create(8), 0))
print("read-empty " .. Try(Lib.readvectorf32x3, buffer.create(0), 0) .. " | " .. Try(Lib.readvectorf32x2, buffer.create(0), 0))
print("read-offset fraction " .. Try(Lib.readvectorf32x3, Source, 4.9) .. " | nan " .. Try(Lib.readvectorf32x3, Source, 0 / 0) .. " | inf " .. Try(Lib.readvectorf32x3, Source, math.huge) .. " | -inf " .. Try(Lib.readvectorf32x3, Source, -math.huge))
print("read-offset 2^31 " .. Try(Lib.readvectorf32x3, Source, 2 ^ 31) .. " | 2^32 " .. Try(Lib.readvectorf32x3, Source, 2 ^ 32) .. " | 2^32+4 " .. Try(Lib.readvectorf32x3, Source, 2 ^ 32 + 4) .. " | -2^31 " .. Try(Lib.readvectorf32x3, Source, -2 ^ 31) .. " | 2^31-4 " .. Try(Lib.readvectorf32x3, Source, 2 ^ 31 - 4) .. " | 1e300 " .. Try(Lib.readvectorf32x3, Source, 1e300))
print("read-offset-string " .. Try(Lib.readvectorf32x3, Source, "4") .. " | " .. Try(Lib.readvectorf32x2, Source, "0x8"))
print("read-extra-arguments " .. Try(Lib.readvectorf32x3, Source, 0, "more", {}) .. " | " .. Try(Lib.readvectorf32x2, Source, 0, nil))

-- what a read keeps: every float32, as readf32 widens it
local Special = buffer.create(24)
buffer.writeu32(Special, 0, 0x7F800000) -- inf
buffer.writeu32(Special, 4, 0xFF800000) -- -inf
buffer.writeu32(Special, 8, 0x7FC00000) -- nan
buffer.writeu32(Special, 12, 0x00000001) -- the smallest subnormal
buffer.writeu32(Special, 16, 0x80000000) -- -0
buffer.writeu32(Special, 20, 0x7F7FFFFF) -- the largest float32
print("read-special " .. Try(Lib.readvectorf32x3, Special, 0) .. " | " .. Try(Lib.readvectorf32x3, Special, 12) .. " | " .. Try(Lib.readvectorf32x2, Special, 12))
print("read-x2-zero-z " .. P.num(Lib.readvectorf32x2(Special, 16).Z) .. " " .. P.num(Lib.readvectorf32x2(Source, 0).Z))
print("read-type " .. type(Lib.readvectorf32x3(Source, 0)) .. " " .. typeof(Lib.readvectorf32x3(Source, 0)) .. " " .. typeof(Lib.readvectorf32x2(Source, 0)) .. " " .. tostring(Lib.readvectorf32x3(Source, 0) == Vector3.new(1.5, -2.25, buffer.readf32(Source, 8))))
print("read-select " .. select("#", Lib.readvectorf32x3(Source, 0)) .. " " .. select("#", Lib.readvectorf32x2(Source, 0)))

-- writes
local function Write(Name, Size, Offset, Value)
	local Target = buffer.create(Size)
	buffer.fill(Target, 0, 0xAA)
	return Try(Lib[Name], Target, Offset, Value) .. " " .. Bytes(Target)
end
print("write-x3 " .. Write("writevectorf32x3", 16, 0, vector.create(1.5, -2.25, 1 / 3)) .. " | " .. Write("writevectorf32x3", 16, 4, Vector3.new(1, 2, 3)))
print("write-x2 " .. Write("writevectorf32x2", 16, 0, vector.create(1.5, -2.25, 99)) .. " | " .. Write("writevectorf32x2", 16, 8, Vector3.new(1, 2, 3)))
print("write-unaligned " .. Write("writevectorf32x3", 16, 3, vector.create(1, 2, 3)) .. " | " .. Write("writevectorf32x2", 16, 5, vector.create(1, 2, 3)))
print("write-x3-bounds last " .. Write("writevectorf32x3", 16, 4, vector.one) .. " | past " .. Write("writevectorf32x3", 16, 5, vector.one) .. " | end " .. Write("writevectorf32x3", 16, 16, vector.one) .. " | negative " .. Write("writevectorf32x3", 16, -1, vector.one))
print("write-x2-bounds last " .. Write("writevectorf32x2", 16, 8, vector.one) .. " | past " .. Write("writevectorf32x2", 16, 9, vector.one) .. " | end " .. Write("writevectorf32x2", 16, 16, vector.one) .. " | negative " .. Write("writevectorf32x2", 16, -8, vector.one))
print("write-short x3 " .. Write("writevectorf32x3", 11, 0, vector.one) .. " | " .. Write("writevectorf32x3", 12, 0, vector.one) .. " | x2 " .. Write("writevectorf32x2", 7, 0, vector.one) .. " | " .. Write("writevectorf32x2", 8, 0, vector.one))
print("write-empty " .. Write("writevectorf32x3", 0, 0, vector.one) .. " | " .. Write("writevectorf32x2", 0, 0, vector.one))
print("write-offset fraction " .. Write("writevectorf32x3", 16, 2.9, vector.one) .. " | nan " .. Write("writevectorf32x3", 16, 0 / 0, vector.one) .. " | inf " .. Write("writevectorf32x2", 16, math.huge, vector.one) .. " | 2^32 " .. Write("writevectorf32x3", 16, 2 ^ 32, vector.one) .. " | string " .. Write("writevectorf32x2", 16, "4", vector.one))
print("write-special " .. Write("writevectorf32x3", 12, 0, vector.create(math.huge, -math.huge, 0 / 0)) .. " | " .. Write("writevectorf32x3", 12, 0, vector.create(-0, 1e-30, 3.4028234663852886e38)) .. " | " .. Write("writevectorf32x2", 8, 0, vector.create(1e40, -1e-50, 0)))
-- a component below the smallest normal float32, as vector.create and writef32 make it (a client makes it zero)
print("write-subnormal " .. Write("writevectorf32x3", 12, 0, vector.create(1e-45, -1e-39, 1.1754942e-38)) .. " | " .. P.try(function()
	local Target = buffer.create(8)
	buffer.writef32(Target, 0, 1e-45)
	buffer.writef32(Target, 4, vector.create(1e-39, 0, 0).X)
	return Bytes(Target)
end))
print("write-results " .. select("#", Lib.writevectorf32x3(buffer.create(12), 0, vector.one)) .. " " .. select("#", Lib.writevectorf32x2(buffer.create(8), 0, vector.one)))

-- a write then a read gives the float32 of each component back
local Round = buffer.create(12)
Lib.writevectorf32x3(Round, 0, vector.create(0.1, -123456.789, 16777217))
print("round-trip " .. Try(Lib.readvectorf32x3, Round, 0) .. " | " .. Try(Lib.readvectorf32x2, Round, 4) .. " | " .. P.f32s(buffer.readf32(Round, 0), buffer.readf32(Round, 4), buffer.readf32(Round, 8)))

-- cases from a generator, summed up: every offset around the end of buffers of every small size
local Sum = 0
local Errors = 0
local Draw = P.rng(747)
for Size = 0, 20 do
	for Offset = -2, Size + 2 do
		local Target = buffer.create(Size)
		local Value = vector.create(Draw.real(-100, 100), Draw.real(-100, 100), Draw.real(-100, 100))
		for _, Width in { 2, 3 } do
			local Wrote = pcall(Lib["writevectorf32x" .. Width], Target, Offset, Value)
			local Read, Back = pcall(Lib["readvectorf32x" .. Width], Target, Offset)
			if Wrote ~= Read then
				print("sweep-disagree " .. Size .. " " .. Offset .. " " .. Width)
			end
			if Read then
				Sum += Back.X + Back.Y * 3 + Back.Z * 7
			else
				Errors += 1
			end
		end
	end
end
print("sweep " .. P.num(Sum) .. " errors " .. Errors)

-- argument errors (checked against an engine only where it has the functions)
if Native then
	local Target = buffer.create(16)
	print("arguments-read none " .. Try(buffer.readvectorf32x3) .. " | no offset " .. Try(buffer.readvectorf32x3, Target) .. " | nil offset " .. Try(buffer.readvectorf32x2, Target, nil) .. " | table offset " .. Try(buffer.readvectorf32x2, Target, {}) .. " | text offset " .. Try(buffer.readvectorf32x3, Target, "four"))
	print("arguments-read-buffer string " .. Try(buffer.readvectorf32x3, "0123456789ab", 0) .. " | nil " .. Try(buffer.readvectorf32x2, nil, 0) .. " | table " .. Try(buffer.readvectorf32x3, {}, 0))
	print("arguments-write none " .. Try(buffer.writevectorf32x3) .. " | no value " .. Try(buffer.writevectorf32x3, Target, 0) .. " | nil value " .. Try(buffer.writevectorf32x2, Target, 0, nil) .. " | no offset " .. Try(buffer.writevectorf32x2, Target))
	print("arguments-write-value number " .. Try(buffer.writevectorf32x3, Target, 0, 1) .. " | string " .. Try(buffer.writevectorf32x3, Target, 0, "1, 2, 3") .. " | table " .. Try(buffer.writevectorf32x2, Target, 0, { 1, 2, 3 }) .. " | Vector2 " .. Try(buffer.writevectorf32x2, Target, 0, Vector2.new(1, 2)) .. " | CFrame " .. Try(buffer.writevectorf32x3, Target, 0, CFrame.identity) .. " | three numbers " .. Try(buffer.writevectorf32x3, Target, 0, 1, 2, 3))
	print("arguments-order bad value out of bounds " .. Try(buffer.writevectorf32x3, Target, 99, 1) .. " | bad offset bad value " .. Try(buffer.writevectorf32x3, Target, {}, 1) .. " | bad buffer bad offset " .. Try(buffer.writevectorf32x3, 1, {}, 1))
	print("arguments-write-untouched " .. Bytes(Target))
	-- (an offset between -1 and 0 is 0, as for readf32; the reference would read its later components one byte early)
	print("arguments-offset-negative-fraction " .. Try(buffer.readvectorf32x3, Source, -0.5) .. " | " .. Try(buffer.readf32, Source, -0.5))
else
	print("arguments (the functions are not in this engine)")
end
