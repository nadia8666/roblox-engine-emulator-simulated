-- math.random and Random by seed, math.noise, and the math library's edge cases.
for _, Seed in { 0, 1, 42, 12345, -7, 2 ^ 31, 2 ^ 32 + 5, 1.5, 123456789 } do
	math.randomseed(Seed)
	local Out = {}
	for _ = 1, 4 do
		table.insert(Out, P.num(math.random()))
	end
	for _ = 1, 4 do
		table.insert(Out, tostring(math.random(100)))
	end
	for _ = 1, 4 do
		table.insert(Out, tostring(math.random(-5, 5)))
	end
	table.insert(Out, P.try(math.random, 2 ^ 40))
	print("math.random seed " .. P.num(Seed) .. " " .. table.concat(Out, " "))
end
print("math.random-errors " .. P.try(math.random, 0) .. " | " .. P.try(math.random, 5, 1) .. " | " .. P.try(math.random, 1.5) .. " | " .. P.try(math.random, 1, 2, 3))
print("math.random-bounds " .. P.try(math.random, -2 ^ 53, 2 ^ 53) .. " | " .. P.try(math.random, 2 ^ 63))

for _, Seed in { 0, 1, 42, 12345, -7, 2 ^ 31, 2 ^ 52, 1.5, 0.25, 123456789 } do
	local Generator = Random.new(Seed)
	local Out = {}
	for _ = 1, 3 do
		table.insert(Out, P.num(Generator:NextNumber()))
	end
	for _ = 1, 3 do
		table.insert(Out, P.num(Generator:NextNumber(-10, 10)))
	end
	for _ = 1, 3 do
		table.insert(Out, tostring(Generator:NextInteger(1, 1000)))
	end
	table.insert(Out, tostring(Generator:NextInteger(-2 ^ 40, 2 ^ 40)))
	local Unit = Generator:NextUnitVector()
	table.insert(Out, "u(" .. P.f32s(Unit.X, Unit.Y, Unit.Z) .. ")")
	local List = { 1, 2, 3, 4, 5, 6, 7, 8 }
	Generator:Shuffle(List)
	table.insert(Out, "shuffle(" .. table.concat(List, ",") .. ")")
	local Clone = Generator:Clone()
	table.insert(Out, "clone " .. tostring(Clone:NextInteger(1, 1e6) == Generator:NextInteger(1, 1e6)))
	print("Random seed " .. P.num(Seed) .. " " .. table.concat(Out, " "))
end
local Generator = Random.new(9)
print("Random-errors " .. P.try(Generator.NextInteger, Generator, 5, 1) .. " | " .. P.try(Generator.NextNumber, Generator, 5, 1) .. " | " .. P.try(Generator.NextInteger, Generator, 1.5, 2.5) .. " | " .. P.try(Generator.Shuffle, Generator, { 1, nil, 3 }) .. " | " .. P.try(Random.new, "x"))
print("Random-intrange " .. P.try(Generator.NextInteger, Generator, -2 ^ 63, 2 ^ 63))
print("Random-tostring " .. tostring(Random.new(1)) .. " typeof " .. typeof(Random.new(1)))
-- math.random after Random: independent streams
math.randomseed(5)
local First = math.random(1, 1e6)
math.randomseed(5)
Random.new(5):NextNumber()
print("math.random-independent " .. tostring(First == math.random(1, 1e6)))

-- math.noise
local Rng = P.rng(99)
local Out = {}
for Index = 1, 48 do
	local X, Y, Z = Rng.real(-50, 50), Rng.real(-50, 50), Rng.real(-50, 50)
	local Kind = Index % 3
	local Value = if Kind == 0 then math.noise(X) elseif Kind == 1 then math.noise(X, Y) else math.noise(X, Y, Z)
	table.insert(Out, P.num(Value))
	if #Out == 12 then
		print("noise " .. table.concat(Out, " "))
		table.clear(Out)
	end
end
print("noise-integers " .. P.nums(math.noise(0), math.noise(1), math.noise(1, 2), math.noise(1, 2, 3), math.noise(0.5), math.noise(-0.5, 0.25), math.noise(1e6 + 0.5), math.noise(1e9 + 0.25)))
print("noise-errors " .. P.try(math.noise) .. " | " .. P.try(math.noise, "x"))

-- the math library
print("math " .. P.nums(math.floor(-0.5), math.ceil(-0.5), math.round(0.5), math.round(-0.5), math.round(2.5), math.round(-2.5), math.sign(-3), math.sign(0), math.sign(-0), math.clamp(5, 1, 3), math.clamp(0 / 0, 1, 3)))
print("math-clamp-error " .. P.try(math.clamp, 1, 3, 1))
print("math-more " .. P.nums(math.fmod(7, -3), math.fmod(-7, 3), 7 % -3, -7 % 3, 5.5 % 2, -5.5 % 2, 1 % 0, math.fmod(1, 0), 2 ^ 0.5, (-8) ^ (1 / 3), 0 ^ 0, math.huge - math.huge))
print("math-trig " .. P.nums(math.sin(1), math.cos(1), math.tan(1), math.asin(0.5), math.acos(0.5), math.atan(1), math.atan2(1, -1), math.atan(1, -1), math.sinh(1), math.cosh(1), math.tanh(1)))
print("math-exp " .. P.nums(math.exp(1), math.log(10), math.log(8, 2), math.log(100, 10), math.log10(1000), math.sqrt(2), math.pow(2, 10), math.log(0), math.log(-1)))
print("math-int " .. P.nums(math.abs(-0), math.max(1, 0 / 0, 3), math.min(0 / 0, 1), math.max(-0, 0), math.min(0, -0), math.modf(-3.7)))
print("math-frexp " .. P.nums(math.frexp(8)) .. " ldexp " .. P.num(math.ldexp(0.5, 4)) .. " map " .. P.try(function()
	return math.map(5, 0, 10, 0, 100)
end) .. " lerp " .. P.try(function()
	return math.lerp(1, 3, 0.25)
end))
print("math-constants " .. P.nums(math.pi, math.huge) .. " " .. tostring(math.maxinteger) .. " " .. tostring(math.mininteger))
print("integer-division " .. P.nums(7 // 2, -7 // 2, 7 // -2, 7.5 // 2, 1 // 0, -1 // 0, 0 // 0))
print("bit32 " .. P.nums(bit32.band(0xFF, 0x0F), bit32.bor(1, 2), bit32.bxor(-1, 0), bit32.lshift(1, 31), bit32.rshift(-1, 28), bit32.arshift(-16, 2), bit32.lrotate(1, 33), bit32.extract(0xF0, 4, 4), bit32.replace(0, 1, 31), bit32.bnot(0), bit32.btest(1, 2), bit32.countlz(1), bit32.countrz(8), bit32.byteswap(0x01020304)))
print("bit32-range " .. P.nums(bit32.band(2 ^ 32 + 1), bit32.band(-1.5), bit32.band(1e20)))
print("bit32-errors " .. P.try(bit32.extract, 1, 30, 4) .. " | " .. P.try(bit32.lshift, 1, "x"))

-- Random:NextInteger's draws, wide ranges and bounds, Random.new's seeds, and the arguments of Random's methods.
-- How many draws one NextInteger takes: its value (less the low bound), then a NextNumber that shows where the stream is.
local function Draws(Seed, Low, Count)
	local Generator = Random.new(Seed)
	local Value = Generator:NextInteger(Low, Low + Count - 1)
	return P.num(Value - Low) .. "/" .. P.num(Generator:NextNumber())
end
for _, Seed in { 7, 42, -3, 123456789 } do
	local Out = {}
	for _, Count in { 2 ^ 32 - 3, 2 ^ 32 - 2, 2 ^ 32 - 1, 2 ^ 32, 2 ^ 32 + 1 } do
		table.insert(Out, Draws(Seed, 0, Count))
		table.insert(Out, Draws(Seed, -2 ^ 31 - 7, Count))
	end
	print("Random-draws seed " .. P.num(Seed) .. " " .. table.concat(Out, " "))
end
-- Two-draw values: ranges of 2^32 - 1 values and wider (powers of two and their neighbours, uneven sizes, wider than 2^53),
-- the first NextInteger of 20 seeds each.
local WideSeeds = { 0, 7, 42, -3, 1.9, -1.9, 4294967303, 910576195, 9007199254740991, -9007199254740991 }
local SeedCases = P.rng(2026)
for _ = 1, 10 do
	local Seed = (SeedCases.u32() % 2097152) * 4294967296 + SeedCases.u32()
	table.insert(WideSeeds, if SeedCases.u32() % 2 == 0 then Seed else -Seed)
end
local WideRanges = {}
for _, Count in {
	2 ^ 32 - 1, 2 ^ 32, 2 ^ 32 + 1, 2 ^ 32 + 2, 2 ^ 33 - 2, 2 ^ 33 - 1, 2 ^ 33, 2 ^ 33 + 1, 2 ^ 33 + 2, 2 ^ 41 - 2, 2 ^ 41 - 1, 2 ^ 41,
	2 ^ 41 + 1, 2 ^ 41 + 2, 2 ^ 41 + 1001, 1e12 + 12346, 1e15 + 78, 2 ^ 52 + 1, 2 ^ 53 - 1, 2 ^ 53,
} do
	table.insert(WideRanges, { 0, Count - 1 })
end
for _, Range in {
	{ -2 ^ 31, 2 ^ 31 - 1 }, { -2 ^ 40, 2 ^ 40 }, { -2 ^ 40 - 3, 2 ^ 40 + 1001 }, { 2 ^ 40, -2 ^ 40 }, { -2 ^ 53, 2 ^ 53 },
	{ -2 ^ 62, 2 ^ 62 }, { 0, 2 ^ 63 - 1024 }, { -2 ^ 63, 2 ^ 63 - 1024 }, { -2 ^ 63, 0 }, { -1e18, 1e18 }, { -5e17, 7e18 },
} do
	table.insert(WideRanges, Range)
end
for _, Range in WideRanges do
	local Out = {}
	for _, Seed in WideSeeds do
		table.insert(Out, P.num(Random.new(Seed):NextInteger(Range[1], Range[2])))
	end
	print("Random-wide " .. P.nums(Range[1], Range[2]) .. " " .. table.concat(Out, " "))
end
for _, Seed in { 7, 910576195 } do
	local Generator = Random.new(Seed)
	local Out = {}
	for _ = 1, 3 do
		table.insert(Out, P.num(Generator:NextInteger(0, 2 ^ 32 - 1)))
		table.insert(Out, P.num(Generator:NextInteger(1, 100)))
		table.insert(Out, P.num(Generator:NextInteger(0, 2 ^ 32 - 2)))
		table.insert(Out, P.num(Generator:NextInteger(-2 ^ 40, 2 ^ 40)))
	end
	table.insert(Out, P.num(Generator:NextNumber()))
	print("Random-sequence seed " .. P.num(Seed) .. " " .. table.concat(Out, " "))
end
-- Fractional bounds, negative and reversed (some change the range's size across 2^32 - 1 values), and bounds out of
-- the 64-bit range.
for _, Case in {
	{ -1.5, -1.5 }, { -0.5, -0.25 }, { -2.7, -2.2 }, { -1.9, 1.9 }, { 10.9, -4.2 }, { 1.5, 2.5 }, { -0.9, -0.1 }, { 2.5, -2.5 },
	{ -4.5, 3.5 }, { -1e9 - 0.5, -1e9 - 0.25 }, { -0.5, 2 ^ 32 - 2 }, { -1.5, 2 ^ 32 - 3 }, { -0.5, 2 ^ 32 - 3 },
	{ -2 ^ 31 - 0.5, 2 ^ 31 - 1.5 }, { -2 ^ 40 - 0.5, 2 ^ 40 + 0.5 },
} do
	local Out = {}
	for _, Seed in { 7, 42 } do
		local Generator = Random.new(Seed)
		table.insert(Out, P.num(Generator:NextInteger(Case[1], Case[2])) .. "/" .. P.num(Generator:NextNumber()))
	end
	print("Random-fraction " .. P.nums(Case[1], Case[2]) .. " " .. table.concat(Out, " "))
end
local Odd = {}
for _, Case in {
	{ 0, math.huge }, { -math.huge, 0 }, { 0, 0 / 0 }, { 0 / 0, 5 }, { 2 ^ 63, 2 ^ 64 }, { 1e300, -1e300 }, { -2 ^ 63, 2 ^ 63 },
	{ -1e19, 0 }, { 0, 1e19 }, { -2 ^ 63, -2 ^ 63 + 4096 }, { 2 ^ 63 - 2048, 2 ^ 63 - 1024 },
} do
	local Generator = Random.new(7)
	table.insert(Odd, P.num(Generator:NextInteger(Case[1], Case[2])) .. "/" .. P.num(Generator:NextNumber()))
end
print("Random-odd-bounds " .. table.concat(Odd, " "))
-- NextInteger over random bounds and seeds (fractional, reversed, every scale): the value and where the stream is after it.
local Cases = P.rng(777)
local function Bound()
	local Kind = Cases.u32() % 8
	local Sign = if Cases.u32() % 2 == 0 then 1 else -1
	if Kind == 0 then
		return Cases.int(-1000, 1000)
	elseif Kind <= 3 then
		return Sign * math.floor(2 ^ (Cases.u32() % 64) * (1 + Cases.u32() / 4294967296))
	elseif Kind == 4 then
		return Cases.int(-1000, 1000) + Cases.real(-0.5, 0.5)
	elseif Kind == 5 then
		return Sign * 2 ^ (31 + Cases.u32() % 3) + Cases.int(-3, 3)
	end
	return 0
end
local Out = {}
for _ = 1, 300 do
	local Low = Bound()
	local Kind = Cases.u32() % 4
	local High = if Kind == 0
		then Bound()
		elseif Kind == 1 then Low + 2 ^ 32 + Cases.int(-3, 2)
		elseif Kind == 2 then Low + math.floor(2 ^ (Cases.u32() % 64) * (1 + Cases.u32() / 4294967296))
		else Low + Cases.int(0, 100)
	if Cases.u32() % 6 == 0 then
		Low, High = High, Low
	end
	if Cases.u32() % 8 == 0 then
		High += Cases.real(-0.5, 0.5)
	end
	local Seed = (Cases.u32() % 2097152) * 4294967296 + Cases.u32()
	local Generator = Random.new(if Cases.u32() % 2 == 0 then Seed else -Seed)
	table.insert(Out, P.num(Generator:NextInteger(Low, High)) .. "/" .. P.num(Generator:NextNumber()))
	if #Out == 10 then
		print("Random-cases " .. table.concat(Out, " "))
		table.clear(Out)
	end
end
-- Seeds of 2^53 or more either way (infinities too), NaN (any payload), and fractional seeds: the first NextNumber.
local function Double(Bytes)
	return (string.unpack("<d", Bytes))
end
local Seeded = {}
for _, Seed in {
	2 ^ 53 - 1, -(2 ^ 53 - 1), 2 ^ 53, -2 ^ 53, 2 ^ 53 + 2, 2 ^ 55, 2 ^ 63, -2 ^ 63, 2 ^ 64, -2 ^ 64, 1e19, -1e19, 1e300,
	math.huge, -math.huge,
} do
	table.insert(Seeded, P.num(Random.new(Seed):NextNumber()))
end
print("Random-seeds-large " .. table.concat(Seeded, " "))
table.clear(Seeded)
for _, Seed in { 0 / 0, -(0 / 0), Double("\0\0\0\0\0\0\248\127"), Double("\1\0\0\0\0\0\240\127"), 2 ^ 52 - 0.5, -(2 ^ 52 - 0.5), -0.5 } do
	table.insert(Seeded, P.num(Random.new(Seed):NextNumber()))
end
print("Random-seeds-nan-fraction " .. table.concat(Seeded, " "))
-- Numeric strings, hex included, for both methods and for Random.new.
local function Colon(Name, ...)
	local Arguments = table.pack(...)
	return P.try(function()
		local Generator = Random.new(7)
		local Value = if Name == "NextInteger"
			then Generator:NextInteger(table.unpack(Arguments, 1, Arguments.n))
			else Generator:NextNumber(table.unpack(Arguments, 1, Arguments.n))
		return Value, Generator:NextNumber()
	end)
end
print("Random-strings " .. table.concat({
	Colon("NextInteger", "5", "10"), Colon("NextInteger", "0x10", "20"), Colon("NextInteger", " 5 ", 10), Colon("NextInteger", 5, "1e1"),
	Colon("NextInteger", "-1.5", "-1.5"), Colon("NextInteger", "0", "4294967294"), Colon("NextNumber", "2", "3"),
	Colon("NextNumber", " 2 ", "0x3"),
}, " | "))
print("Random-string-seeds " .. P.try(function()
	return Random.new("5"):NextNumber(), Random.new("0x10"):NextNumber(), Random.new(" 7 "):NextNumber(), Random.new("1e3"):NextNumber()
end))
-- Argument errors: a `.` call through pcall (no position), the same `.` call from a function (at the calling line), and a
-- `:` call (at the calling line, without the method's name).
local Generator = Random.new(7)
local Wrong = {
	{ "NextInteger", n = 0 }, { "NextInteger", 1, n = 1 }, { "NextInteger", nil, 5, n = 2 }, { "NextInteger", 5, nil, n = 2 },
	{ "NextInteger", "abc", 1, n = 2 }, { "NextInteger", 1, "abc", n = 2 }, { "NextInteger", true, 1, n = 2 }, { "NextInteger", {}, 1, n = 2 },
	{ "NextInteger", Vector3.zero, 1, n = 2 }, { "NextInteger", game, 1, n = 2 }, { "NextNumber", 5, n = 1 }, { "NextNumber", nil, n = 1 },
	{ "NextNumber", "abc", n = 1 }, { "NextNumber", nil, 5, n = 2 }, { "NextNumber", 1, "abc", n = 2 }, { "NextNumber", 1, 2, 3, n = 3 },
	{ "Shuffle", n = 0 }, { "Shuffle", nil, n = 1 }, { "Shuffle", 5, n = 1 },
}
local Dotted, Direct = {}, {}
for _, Case in Wrong do
	table.insert(Dotted, P.try(Generator[Case[1]], Generator, table.unpack(Case, 2, 1 + Case.n)))
	table.insert(Direct, P.try(function()
		return Generator[Case[1]](Generator, table.unpack(Case, 2, 1 + Case.n))
	end))
end
print("Random-arguments-dot " .. table.concat(Dotted, " | "))
print("Random-arguments-dot-direct " .. table.concat(Direct, " | "))
print("Random-arguments-colon " .. table.concat({
	P.try(function()
		return Generator:NextInteger()
	end),
	P.try(function()
		return Generator:NextInteger(1)
	end),
	P.try(function()
		return Generator:NextInteger(nil, 5)
	end),
	P.try(function()
		return Generator:NextInteger(5, nil)
	end),
	P.try(function()
		return Generator:NextInteger("abc", 1)
	end),
	P.try(function()
		return Generator:NextInteger(1, "abc")
	end),
	P.try(function()
		return Generator:NextInteger(game, 1)
	end),
	P.try(function()
		return Generator:NextNumber(5)
	end),
	P.try(function()
		return Generator:NextNumber(nil)
	end),
	P.try(function()
		return Generator:NextNumber(nil, 5)
	end),
	P.try(function()
		return Generator:NextNumber(1, {})
	end),
	P.try(function()
		return Generator:Shuffle()
	end),
	P.try(function()
		return Generator:Shuffle(5)
	end),
	P.try(function()
		return Generator:Shuffle({ 1, nil, 3 })
	end),
}, " | "))
print("Random-self " .. table.concat({
	P.try(Generator.NextInteger, nil, 1, 2), P.try(Generator.NextInteger), P.try(Generator.NextNumber, game), P.try(Generator.NextUnitVector, {}),
	P.try(Generator.Clone, 5), P.try(Generator.Clone), P.try(Generator.Shuffle, "x", {}),
}, " | "))
print("Random-new-arguments " .. table.concat({
	P.try(function()
		return Random.new("abc")
	end),
	P.try(Random.new, true),
	P.try(Random.new, {}),
	P.try(Random.new, Vector3.zero),
	P.try(function()
		return typeof(Random.new(nil))
	end),
	P.try(function()
		return Random.new(5, 6):NextNumber()
	end),
}, " | "))
-- The values the engine's own Random tests check (src/lua/types.rs random_matches_the_luau_oracle, src/lua/datatype_tests.rs
-- random_streams_ranges_identity_and_seeds): three NextNumber, NextInteger(1, 100) and a wide NextInteger per seed, then
-- checksums of 256 NextInteger calls by seed (a range of 2^32 - 1 values) and by range (seed 7).
local Vectors = {}
for _, Seed in { 0, 7, -3, 1.9, -1.9, 4294967303, 9007199254740991, -9007199254740991 } do
	local Generator = Random.new(Seed)
	local Doubles = P.nums(Generator:NextNumber(), Generator:NextNumber(), Generator:NextNumber())
	table.insert(Vectors, Doubles .. " " .. P.nums(Generator:NextInteger(1, 100), Generator:NextInteger(-1099511627776, 1099511628776)))
end
print("Random-test-vectors " .. table.concat(Vectors, " | "))
local function Checksum(Seed, Low, High)
	local Generator = Random.new(Seed)
	local Sum = 0
	for _ = 1, 256 do
		Sum = bit32.bxor(bit32.lrotate(Sum, 5), Generator:NextInteger(Low, High))
	end
	return string.format("%08x", Sum)
end
local StreamSeeds = {
	0, 1, -1, 65535, 65536, 65537, 4294967295, 4294967296, 4294967297, -4294967295, -4294967296, -4294967297, 9007199254740991,
	-9007199254740991, 8279140260984440, -3446241584225153, 7966395542197526, -5430052476903239, 2935106855172324,
	-6384148386864253, 7162472545633634, -4177724348780707, 5977007115012624, -7192467481081145, 8863833131590254,
	-1769047220330431, 3184271431751164, -77273909666229, 7432852227569722, -7968050973091995, 5541507801188520,
	-188557386145265, 7918227809178310, -556657445603127, 131636434112020, -5830691134858733, 617780101152274,
	-6276599599929235, 6028669383217728, -4391824743921065, 2734820613752350, -2778718176081327, 2780458655416620,
	-4009636752066853, 1564623516530410, -6651977796910475, 924297076672216, -119958211812449,
}
local Sums = {}
for Index, Seed in StreamSeeds do
	table.insert(Sums, Checksum(Seed, 0, 4294967294))
	if #Sums == 12 then
		print("Random-test-streams " .. (Index - 11) .. "-" .. Index .. " " .. table.concat(Sums, " "))
		table.clear(Sums)
	end
end
for _, Case in {
	{ 0, 0 }, { 1, 12 }, { -1.9, 1.9 }, { 10.9, -4.2 }, { 0, 2097150 }, { 0, 2097151 }, { 0, 2097152 }, { -1048576, 1048575 },
	{ -1048576, 1048576 }, { 4294967296, 4294967306 },
} do
	table.insert(Sums, Checksum(7, Case[1], Case[2]))
end
print("Random-test-ranges " .. table.concat(Sums, " "))
