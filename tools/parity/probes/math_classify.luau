-- math.isnan, math.isinf and math.isfinite (Luau 0.701's number classifiers): the math table's members, the results for finite,
-- infinite, NaN and signed-zero inputs (made at run time, folded by the compiler, and through a C call), the strings the argument
-- check coerces, the errors for everything else, and a sweep of doubles made from random bit patterns.
local Keys = {}
for Key in math do
	table.insert(Keys, Key)
end
table.sort(Keys)
print("math-keys " .. table.concat(Keys, " "))
for _, Name in { "isnan", "isinf", "isfinite" } do
	print("math-member " .. Name .. " " .. type(math[Name]) .. " " .. P.try(function()
		local Source, Line, Named, Arity, Variadic = debug.info(math[Name], "slna")
		return Source, Line, Named, Arity, Variadic
	end))
end

-- values made at run time (a table's fields are never folded by the compiler). Subnormal numbers come straight from a field and
-- are not printed: the client treats a subnormal operand of arithmetic or a comparison as zero, ree does not (the datatypes
-- probe's v3-precision line), which is not what this probe is about.
local K = { Zero = 0, One = 1, Big = 1e308, Huge = math.huge, Two = 2, Sub = 5e-324, NegSub = -5e-324, Normal = 2.2250738585072014e-308 }
local Values = {
	{ "0", K.Zero },
	{ "-0", -K.Zero },
	{ "1", K.One },
	{ "-1", -K.One },
	{ "0.5", K.One / 2 },
	{ "pi", math.pi * K.One },
	{ "2^53", K.Two ^ 53 },
	{ "1e308", K.Big },
	{ "-1e308", -K.Big },
	{ "max", 1.7976931348623157e308 * K.One },
	{ "min-normal", K.Normal },
	{ "min-subnormal", K.Sub, "subnormal" },
	{ "-min-subnormal", K.NegSub, "subnormal" },
	{ "inf", K.Huge },
	{ "-inf", -K.Huge },
	{ "overflow", K.Big * 10 },
	{ "-overflow", K.Big * -10 },
	{ "underflow", 5e-324 * K.One / 2 },
	{ "1/0", K.One / K.Zero },
	{ "-1/0", -K.One / K.Zero },
	{ "0/0", K.Zero / K.Zero },
	{ "-(0/0)", -(K.Zero / K.Zero) },
	{ "inf-inf", K.Huge - K.Huge },
	{ "inf*0", K.Huge * K.Zero },
	{ "sqrt(-1)", math.sqrt(-K.One) },
	{ "(-2)^0.5", (-K.Two) ^ 0.5 },
	{ "log(0)", math.log(K.Zero) },
	{ "log(-1)", math.log(-K.One) },
	{ "fmod(1,0)", math.fmod(K.One, K.Zero) },
}
local Calls = { pcall = 0, field = 0, alias = 0 }
local Table = { isnan = math.isnan, isinf = math.isinf, isfinite = math.isfinite }
for _, Entry in Values do
	local Label, Value = Entry[1], Entry[2]
	local Fast = { math.isnan(Value), math.isinf(Value), math.isfinite(Value) }
	print("math-class " .. Label .. " " .. (Entry[3] or P.num(Value)) .. " " .. tostring(Fast[1]) .. " " .. tostring(Fast[2]) .. " " .. tostring(Fast[3]))
	-- the same through a C call: pcall, a table's field (never a fast call) and a local alias
	for Index, Name in { "isnan", "isinf", "isfinite" } do
		local Ok, Result = pcall(math[Name], Value)
		if Ok and Result == Fast[Index] then
			Calls.pcall += 1
		end
		if Table[Name](Value) == Fast[Index] then
			Calls.field += 1
		end
	end
	local IsNan, IsInf, IsFinite = math.isnan, math.isinf, math.isfinite
	if IsNan(Value) == Fast[1] and IsInf(Value) == Fast[2] and IsFinite(Value) == Fast[3] then
		Calls.alias += 3
	end
end
print("math-class-calls of " .. #Values * 3 .. ": pcall " .. Calls.pcall .. " field " .. Calls.field .. " alias " .. Calls.alias)
-- constant arguments the compiler may fold
print("math-folded " .. tostring(math.isnan(0 / 0)) .. " " .. tostring(math.isnan(0)) .. " " .. tostring(math.isinf(math.huge)) .. " " .. tostring(math.isinf(-4))
	.. " " .. tostring(math.isinf(-1 / 0)) .. " " .. tostring(math.isfinite(42)) .. " " .. tostring(math.isfinite(-math.huge)) .. " " .. tostring(math.isfinite(1e308 * 10))
	.. " " .. tostring(math.isnan(-(0 / 0))) .. " " .. tostring(math.isfinite(-0)))
-- results and their count
print("math-results " .. select("#", math.isnan(1)) .. " " .. select("#", math.isinf(1, 2, 3)) .. " " .. select("#", (math.isfinite(1))) .. " " .. P.try(function()
	return math.isnan(0 / K.Zero, "extra"), math.isinf(K.Huge, nil), math.isfinite(K.One, {})
end))

-- strings the argument check coerces (luaL_checknumber), and the ones it refuses
local Strings = { "1", " 1 ", "1.5", "-0", "0x10", "0X1p4", "0b101", "1e400", "-1e400", "1e-400", "inf", "-inf", "nan", "NaN", "infinity", "1e", "", " ", "abc", "1 2", "0/0", "1_000" }
for _, Text in Strings do
	print("math-coerce " .. string.format("%q", Text) .. " isnan " .. P.try(math.isnan, Text) .. " | isinf " .. P.try(math.isinf, Text) .. " | isfinite " .. P.try(math.isfinite, Text))
end
-- arguments that are not numbers
local Part = Instance.new("Part")
local Others = {
	{ "none", table.pack() },
	{ "nil", table.pack(nil) },
	{ "true", table.pack(true) },
	{ "false", table.pack(false) },
	{ "table", table.pack({}) },
	{ "function", table.pack(print) },
	{ "Vector3", table.pack(Vector3.new(1, 2, 3)) },
	{ "Vector2", table.pack(Vector2.new(1, 2)) },
	{ "CFrame", table.pack(CFrame.new()) },
	{ "Instance", table.pack(Part) },
	{ "EnumItem", table.pack(Enum.Material.Plastic) },
	{ "buffer", table.pack(buffer.create(1)) },
	{ "thread", table.pack(coroutine.create(print)) },
	{ "string", table.pack("x") },
}
for _, Entry in Others do
	local Arguments = Entry[2]
	print("math-error " .. Entry[1] .. " isnan " .. P.try(function()
		return math.isnan(table.unpack(Arguments, 1, Arguments.n))
	end) .. " | isinf " .. P.try(math.isinf, table.unpack(Arguments, 1, Arguments.n)) .. " | isfinite " .. P.try(function()
		return Table.isfinite(table.unpack(Arguments, 1, Arguments.n))
	end))
end
Part:Destroy()

-- doubles from random bit patterns (one in four with the exponent all ones, one in four all zeros): the classifiers against
-- their definitions
local Rng = P.rng(701)
local Counts = { nan = 0, inf = 0, finite = 0, agree = 0, n = 0 }
for _ = 1, 400 do
	local Low = Rng.u32()
	local High = Rng.u32()
	local Pick = Rng.int(1, 4)
	if Pick == 1 then
		High = bit32.bor(High, 0x7FF00000)
	elseif Pick == 2 then
		High = bit32.band(High, 0x800FFFFF)
	end
	if Rng.int(1, 8) == 1 then
		Low = 0
		High = bit32.band(High, 0xFFF00000)
	end
	local Value = string.unpack("<d", string.pack("<I4I4", Low, High))
	local Nan, Inf, Finite = math.isnan(Value), math.isinf(Value), math.isfinite(Value)
	Counts.n += 1
	Counts.nan += if Nan then 1 else 0
	Counts.inf += if Inf then 1 else 0
	Counts.finite += if Finite then 1 else 0
	if Nan == (Value ~= Value) and Inf == (Value == math.huge or Value == -math.huge) and Finite == (Value == Value and Value ~= math.huge and Value ~= -math.huge) then
		Counts.agree += 1
	end
end
print("math-random " .. Counts.n .. " nan " .. Counts.nan .. " inf " .. Counts.inf .. " finite " .. Counts.finite .. " agree " .. Counts.agree)
