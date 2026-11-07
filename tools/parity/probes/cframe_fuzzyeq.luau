-- CFrame:FuzzyEq(other, eps = 1e-5): position compared per component, rotation by how far the two matrices are apart, the
-- eps argument's range check and the argument errors. Rotations near a threshold are built from integer quaternions, whose
-- matrix entries are exact ratios (the 12-number constructor rounds each to a float the same way in every engine), so a case
-- tests FuzzyEq and not how an engine builds a rotation.
local function Q(W, X, Y, Z, Px, Py, Pz)
	local N = W * W + X * X + Y * Y + Z * Z
	return CFrame.new(Px or 0, Py or 0, Pz or 0,
		(W * W + X * X - Y * Y - Z * Z) / N, 2 * (X * Y - W * Z) / N, 2 * (X * Z + W * Y) / N,
		2 * (X * Y + W * Z) / N, (W * W - X * X + Y * Y - Z * Z) / N, 2 * (Y * Z - W * X) / N,
		2 * (X * Z - W * Y) / N, 2 * (Y * Z + W * X) / N, (W * W - X * X - Y * Y + Z * Z) / N)
end
-- the Hamilton product of two integer quaternions (exact while the components stay below 2^26)
local function Mul(A, B)
	return {
		A[1] * B[1] - A[2] * B[2] - A[3] * B[3] - A[4] * B[4],
		A[1] * B[2] + A[2] * B[1] + A[3] * B[4] - A[4] * B[3],
		A[1] * B[3] - A[2] * B[4] + A[3] * B[1] + A[4] * B[2],
		A[1] * B[4] + A[2] * B[3] - A[3] * B[2] + A[4] * B[1],
	}
end
local function Bits(List)
	local Out = {}
	for _, Value in List do
		table.insert(Out, if Value == true then "1" elseif Value == false then "0" else "?")
	end
	return table.concat(Out)
end
local I = CFrame.identity

-- the default eps (1e-5) on each axis (an eps of nil given is an error: the call list below)
local Default = {}
for _, Offset in { 0.99e-5, 1e-5, 1.0000001e-5, 1.01e-5, 1.1e-5, -1e-5, -1.01e-5 } do
	table.insert(Default, CFrame.new(Offset, 0, 0):FuzzyEq(I))
	table.insert(Default, I:FuzzyEq(CFrame.new(0, Offset, 0)))
	table.insert(Default, CFrame.new(0, 0, Offset):FuzzyEq(I))
end
print("fuzzy-default " .. Bits(Default) .. " " .. tostring(I:FuzzyEq(I)))

-- position per component with an eps that floats and doubles both hold exactly (0.0625): equal, one float over, one under
local Step = 0.0625
local Over, Under = 0.062500007450580597, 0.062499996274709702
local Position = {}
for Axis = 1, 3 do
	for _, Offset in { Step, Over, Under, -Step, -Over } do
		local V = { 0, 0, 0 }
		V[Axis] = Offset
		table.insert(Position, CFrame.new(V[1], V[2], V[3]):FuzzyEq(I, Step))
		V[Axis] = 3.5 + Offset
		table.insert(Position, CFrame.new(V[1], V[2], V[3]):FuzzyEq(CFrame.new(Axis == 1 and 3.5 or 0, Axis == 2 and 3.5 or 0, Axis == 3 and 3.5 or 0), Step))
	end
end
print("fuzzy-position " .. Bits(Position))
-- all three axes just inside (not a distance), and one just outside
print("fuzzy-position-axes " .. Bits({
	CFrame.new(0.0099, 0.0099, 0.0099):FuzzyEq(I, 0.01),
	CFrame.new(0.0099, -0.0099, 0.0101):FuzzyEq(I, 0.01),
	CFrame.new(0.0625, 0.0625, 0.0625):FuzzyEq(I, Step),
	CFrame.new(-0.0625, 0.0625, Over):FuzzyEq(I, Step),
}))
-- the tolerance grows with self's component (|eps| * (|a| + 1), whatever the other's): exactly at it and one float over, for
-- a self at 0 and at 1, either way round; infinite and huge components
local Ulp = 2 ^ -23
print("fuzzy-position-tolerance " .. Bits({
	I:FuzzyEq(CFrame.new(Step, 0, 0), Step),
	CFrame.new(Step, 0, 0):FuzzyEq(I, Step),
	I:FuzzyEq(CFrame.new(0, -Over, 0), Step),
	CFrame.new(1, 0, 0):FuzzyEq(CFrame.new(1.125, 0, 0), Step),
	CFrame.new(1, 0, 0):FuzzyEq(CFrame.new(1.125 + Ulp, 0, 0), Step),
	CFrame.new(1, 0, 0):FuzzyEq(CFrame.new(0.875, 0, 0), Step),
	CFrame.new(1, 0, 0):FuzzyEq(CFrame.new(0.875 - Ulp, 0, 0), Step),
	CFrame.new(1.125, 0, 0):FuzzyEq(CFrame.new(1, 0, 0), Step),
	CFrame.new(0, 0, -3):FuzzyEq(CFrame.new(0, 0, -3.25), Step),
	CFrame.new(0, 0, -3.25):FuzzyEq(CFrame.new(0, 0, -3), Step),
	CFrame.new(0, 0, -3):FuzzyEq(CFrame.new(0, 0, -3.25 - 4 * Ulp), Step),
	CFrame.new(math.huge, 0, 0):FuzzyEq(CFrame.new(3e38, 0, 0), 0.1),
	CFrame.new(-math.huge, 0, 0):FuzzyEq(CFrame.new(-math.huge, 0, 0)),
	CFrame.new(3e38, 0, 0):FuzzyEq(CFrame.new(2.8e38, 0, 0), 0.1),
	CFrame.new(2.8e38, 0, 0):FuzzyEq(CFrame.new(3e38, 0, 0), 0.1),
	CFrame.new(3.4e38, 0, 0):FuzzyEq(CFrame.new(-3.4e38, 0, 0), 0.1),
	CFrame.new(0, 1e6, 0):FuzzyEq(CFrame.new(0, 1e6 + 9, 0)),
	CFrame.new(0, 1e6, 0):FuzzyEq(CFrame.new(0, 1e6 + 11, 0)),
}))
-- an eps a float rounds up to the offset while the double is below it (0.01 as a float is 0.0099999997764825821)
local Near = {}
for _, Eps in { 0.0099999997, 0.00999999975, 0.0099999999, 0.01, 0.0099999988, 0.0099999993, 0.0099999994 } do
	table.insert(Near, CFrame.new(0.01, 0, 0):FuzzyEq(I, Eps))
	table.insert(Near, I:FuzzyEq(CFrame.new(0, -0.01, 0), Eps))
end
print("fuzzy-eps-rounding " .. Bits(Near))

-- rotation about X, Y, Z and (1, 1, 1) by angles near eps (the factors are the angle over eps)
local W = 10 ^ 7
local Rotations = {
	{ 0.1, { 495405, 500367, 500417, 500422, 500467, 500517, 500567, 500618, 500623, 500628, 500643, 500668, 500918, 505430 },
		{ 286022, 288887, 288916, 288919, 288945, 288974, 289003, 289032, 289035, 289037, 289046, 289061, 289205, 291810 } },
	{ 0.03, { 148511, 149996, 150011, 150013, 150014, 150016, 150017, 150019, 150026, 150161, 151512 },
		{ 85743, 86600, 86609, 86610, 86611, 86612, 86613, 86618, 86696, 87475 } },
	{ 0.01, { 49500, 49950, 49995, 50000, 50001, 50005, 50050, 50500 }, { 28579, 28839, 28865, 28868, 28871, 28897, 29156 } },
	{ 0.001, { 4950, 4995, 5000, 5005, 5050 }, { 2858, 2884, 2887, 2890, 2916 } },
}
local Base = { 3, 1, -2, 1 }
for _, Set in Rotations do
	local Eps, Single, Diagonal = Set[1], Set[2], Set[3]
	local Out = {}
	for Axis = 1, 3 do
		local Results = {}
		for _, X in Single do
			local Delta = { W, 0, 0, 0 }
			Delta[Axis + 1] = X
			table.insert(Results, Q(table.unpack(Delta)):FuzzyEq(I, Eps))
		end
		table.insert(Out, Bits(Results))
	end
	local Results, Rotated, Back = {}, {}, {}
	for _, X in Diagonal do
		table.insert(Results, I:FuzzyEq(Q(W, X, X, X), Eps))
	end
	table.insert(Out, Bits(Results))
	-- the same deltas from a rotated base, either way round
	for _, X in Single do
		local Product = Mul(Base, { W, 0, X, 0 })
		table.insert(Rotated, Q(table.unpack(Base)):FuzzyEq(Q(table.unpack(Product)), Eps))
		table.insert(Back, Q(table.unpack(Product)):FuzzyEq(Q(table.unpack(Base)), Eps))
	end
	table.insert(Out, Bits(Rotated))
	table.insert(Out, Bits(Back))
	print("fuzzy-rotation " .. Eps .. " " .. table.concat(Out, " "))
end

-- matrices that are not rotations (the 12-number constructor keeps them): a stretched axis, all axes, a shear
local Stretch = {}
for _, D in { 0.14, 0.1414, 0.14142, 0.141421, 0.141422, 0.14143, 0.1415, 0.15 } do
	table.insert(Stretch, CFrame.new(0, 0, 0, 1 + D, 0, 0, 0, 1, 0, 0, 0, 1):FuzzyEq(I, 0.1))
	table.insert(Stretch, CFrame.new(0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1 - D):FuzzyEq(I, 0.1))
end
local Uniform = {}
for _, D in { 0.08, 0.0816, 0.08164, 0.081649, 0.08165, 0.0817, 0.082 } do
	table.insert(Uniform, CFrame.new(0, 0, 0, 1 + D, 0, 0, 0, 1 + D, 0, 0, 0, 1 + D):FuzzyEq(I, 0.1))
	table.insert(Uniform, CFrame.new(0, 0, 0, 1 - D, 0, 0, 0, 1 - D, 0, 0, 0, 1 - D):FuzzyEq(I, 0.1))
end
local Shear = {}
for _, D in { 0.14, 0.1414, 0.14142, 0.141422, 0.1415, 0.15 } do
	table.insert(Shear, CFrame.new(0, 0, 0, 1, D, 0, 0, 1, 0, 0, 0, 1):FuzzyEq(I, 0.1))
	table.insert(Shear, CFrame.new(0, 0, 0, 1, 0, 0, 0, 1, 0, D, 0, 1):FuzzyEq(I, 0.1))
end
local Twice = {}
for _, D in { 0.099, 0.1, 0.10001, 0.1001 } do
	table.insert(Twice, CFrame.new(0, 0, 0, 1 + D, 0, 0, 0, 1 - D, 0, 0, 0, 1):FuzzyEq(I, 0.1))
	table.insert(Twice, CFrame.new(0, 0, 0, 1, D, 0, -D, 1, 0, 0, 0, 1):FuzzyEq(I, 0.1))
end
print("fuzzy-matrix stretch " .. Bits(Stretch) .. " uniform " .. Bits(Uniform) .. " shear " .. Bits(Shear) .. " two " .. Bits(Twice))

-- position and rotation are checked apart: 0.9 eps of both, then 1.1 eps of one (a turn of 0.009 is X = 45000 at W = 10^7)
print("fuzzy-both " .. Bits({
	Q(W, 45000, 0, 0, 0.009, 0, 0):FuzzyEq(I, 0.01),
	Q(W, 45000, 0, 0, 0.009, -0.009, 0.009):FuzzyEq(I, 0.01),
	Q(W, 0, 45000, 0, 0, 0.0099, 0):FuzzyEq(I, 0.01),
	Q(W, 55001, 0, 0, 0.009, 0, 0):FuzzyEq(I, 0.01),
	Q(W, 45000, 0, 0, 0.011, 0, 0):FuzzyEq(I, 0.01),
}))

-- the eps argument: its range, zero, negative, NaN, huge, tiny
local Close = CFrame.new(1e-6, 0, 0)
for _, Entry in {
	{ "0", 0 }, { "-0", -0 }, { "-0.0001", -0.0001 }, { "-0.1", -0.1 }, { "-0.10000001", -0.10000001 }, { "-1", -1 },
	{ "1e-30", 1e-30 }, { "1e-50", 1e-50 }, { "0.1", 0.1 }, { "0.1000000001", 0.1000000001 }, { "0.1000000015", 0.1000000015 },
	{ "0.10000001", 0.10000001 }, { "0.1000001", 0.1000001 }, { "0.5", 0.5 }, { "1", 1 }, { "inf", math.huge }, { "-inf", -math.huge },
	{ "nan", 0 / 0 }, { "2^-149", 2 ^ -149 }, { "2^-150", 2 ^ -150 },
} do
	print("fuzzy-eps " .. Entry[1] .. " same " .. P.try(function()
		return I:FuzzyEq(I, Entry[2])
	end) .. " | close " .. P.try(function()
		return Close:FuzzyEq(I, Entry[2])
	end) .. " | turned " .. P.try(function()
		return Q(W, 5, 0, 0):FuzzyEq(I, Entry[2])
	end))
end

-- NaN and infinite components
local Nan = 0 / 0
print("fuzzy-nan " .. Bits({
	CFrame.new(Nan, 0, 0):FuzzyEq(CFrame.new(Nan, 0, 0)),
	CFrame.new(Nan, 0, 0):FuzzyEq(I),
	I:FuzzyEq(CFrame.new(0, Nan, 0), 0.1),
	CFrame.new(math.huge, 0, 0):FuzzyEq(CFrame.new(math.huge, 0, 0)),
	CFrame.new(math.huge, 0, 0):FuzzyEq(I, 0.1),
	CFrame.new(0, 0, 0, Nan, 0, 0, 0, 1, 0, 0, 0, 1):FuzzyEq(I, 0.1),
	CFrame.new(0, 0, 0, Nan, 0, 0, 0, 1, 0, 0, 0, 1):FuzzyEq(CFrame.new(0, 0, 0, Nan, 0, 0, 0, 1, 0, 0, 0, 1), 0.1),
	CFrame.new(-0, -0, -0):FuzzyEq(I, 0),
	CFrame.new(1e10, 0, 0):FuzzyEq(CFrame.new(1e10 + 1000, 0, 0), 0.1),
	CFrame.new(1e10, 0, 0):FuzzyEq(CFrame.new(1e10 + 400, 0, 0), 0.1),
}))

-- argument errors and the forms of the call
local Part = Instance.new("Part")
for _, Entry in {
	{ "other none", function() return I:FuzzyEq() end },
	{ "other nil", function() return I:FuzzyEq(nil) end },
	{ "other number", function() return I:FuzzyEq(5) end },
	{ "other string", function() return I:FuzzyEq("x") end },
	{ "other table", function() return I:FuzzyEq({}) end },
	{ "other Vector3", function() return I:FuzzyEq(Vector3.zero) end },
	{ "other Instance", function() return I:FuzzyEq(Part) end },
	{ "eps nil", function() return I:FuzzyEq(I, nil) end },
	{ "eps nil far", function() return I:FuzzyEq(Close, nil) end },
	{ "eps string", function() return I:FuzzyEq(I, "abc") end },
	{ "eps numeric string", function() return I:FuzzyEq(Close, "0.01") end },
	{ "eps numeric string small", function() return Close:FuzzyEq(I, "1e-7") end },
	{ "eps boolean", function() return I:FuzzyEq(I, true) end },
	{ "eps false", function() return I:FuzzyEq(I, false) end },
	{ "eps table", function() return I:FuzzyEq(I, {}) end },
	{ "eps Vector3", function() return I:FuzzyEq(I, Vector3.zero) end },
	{ "extra", function() return I:FuzzyEq(I, 0.01, "extra") end },
	{ "dot nil", function() return I.FuzzyEq(nil, I) end },
	{ "dot number", function() return I.FuzzyEq(5, I) end },
	{ "dot Vector3", function() return I.FuzzyEq(Vector3.zero, I) end },
	{ "dot none", function() return I.FuzzyEq() end },
	{ "dot both", function() return I.FuzzyEq(I, I) end },
	{ "first bad self", function() return I.FuzzyEq(5, nil) end },
	{ "first bad self eps", function() return I.FuzzyEq(5, I, "x") end },
	{ "first count", function() return I.FuzzyEq(5) end },
	{ "first other eps", function() return I:FuzzyEq(nil, "x") end },
	{ "first other range", function() return I:FuzzyEq(nil, 5) end },
	{ "range numeric string", function() return I:FuzzyEq(I, "5") end },
	{ "static", function() return type(CFrame.FuzzyEq) end },
	{ "results", function() return select("#", I:FuzzyEq(I)) end },
} do
	print("fuzzy-call " .. Entry[1] .. " " .. P.try(Entry[2]))
end
Part:Destroy()

-- random pairs near the thresholds (no trigonometry: every input is made by basic arithmetic): rotation only, position only, and
-- both, for each eps; each line is the results in order
local Rng = P.rng(1105)
local function Axis()
	local X, Y, Z
	repeat
		X, Y, Z = Rng.int(-1000, 1000), Rng.int(-1000, 1000), Rng.int(-1000, 1000)
	until X * X + Y * Y + Z * Z > 10000
	local Length = math.sqrt(X * X + Y * Y + Z * Z)
	return X / Length, Y / Length, Z / Length
end
for _, Eps in { 0.1, 0.05, 0.03, 0.01, 0.003, 0.001, 0.0001, 1e-5 } do
	local Turned, Moved, Mixed = {}, {}, {}
	for _ = 1, 40 do
		-- a rotation by about Eps * Factor from a random base
		local Factor = Rng.real(0.97, 1.03)
		local Ax, Ay, Az = Axis()
		local Big = Rng.int(1000000, 2000000)
		local Half = Big * Eps * Factor / 2
		local Delta = { Big, math.floor(Half * Ax + 0.5), math.floor(Half * Ay + 0.5), math.floor(Half * Az + 0.5) }
		local BaseQ = { Rng.int(-9, 9), Rng.int(-9, 9), Rng.int(-9, 9), Rng.int(1, 9) }
		local Px, Py, Pz = Rng.int(-50000, 50000) / 1024, Rng.int(-50000, 50000) / 1024, Rng.int(-50000, 50000) / 1024
		local A = Q(BaseQ[1], BaseQ[2], BaseQ[3], BaseQ[4], Px, Py, Pz)
		local B = Q(table.unpack(Mul(BaseQ, Delta)))
		B = B + Vector3.new(Px, Py, Pz)
		table.insert(Turned, A:FuzzyEq(B, Eps))
		-- a move by about Eps * Factor on one axis (or all three)
		local Move = Eps * Rng.real(0.97, 1.03)
		local Pick = Rng.int(1, 4)
		local Offset = Vector3.new(if Pick == 1 or Pick == 4 then Move else 0, if Pick == 2 or Pick == 4 then -Move else 0, if Pick == 3 or Pick == 4 then Move else 0)
		local Small = Rng.int(-1024, 1024) / 1024
		local C = CFrame.new(Small, -Small, Small * 2)
		table.insert(Moved, C:FuzzyEq(C + Offset, Eps))
		-- both near their thresholds
		table.insert(Mixed, A:FuzzyEq(B + Vector3.new(Eps * Rng.real(0.97, 1.03), 0, 0), Eps))
	end
	print("fuzzy-random " .. Eps .. " turned " .. Bits(Turned) .. " moved " .. Bits(Moved) .. " both " .. Bits(Mixed))
end
