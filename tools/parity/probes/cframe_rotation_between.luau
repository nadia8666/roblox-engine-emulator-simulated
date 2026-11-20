-- CFrame.fromRotationBetweenVectors(a, b) for vectors as given (not made unit): axis-aligned, scaled, random, exactly
-- opposite, nearly opposite across the point where it turns to a half turn (at several lengths, and with the two lengths
-- apart), nearly equal, lengths whose squares leave the float range, zero and not-a-number. Each line prints both
-- vectors as stored and the whole CFrame, so the rule can be checked from the lines alone.
local function V(Vector)
	return P.nums(Vector.X, Vector.Y, Vector.Z)
end
local function Line(Label, A, B)
	local Ok, Result = pcall(function()
		return CFrame.fromRotationBetweenVectors(A, B)
	end)
	print(Label .. " | " .. V(A) .. " | " .. V(B) .. " | " .. (if Ok then P.nums(Result:GetComponents()) else "error " .. P.err(Result)))
end
print("exists " .. typeof(CFrame.fromRotationBetweenVectors))
-- 1 the signed axes against each other
local Axes = { Vector3.xAxis, Vector3.yAxis, Vector3.zAxis, -Vector3.xAxis, -Vector3.yAxis, -Vector3.zAxis }
for I, A in Axes do
	for J, B in Axes do
		Line("axes " .. I .. " " .. J, A, B)
	end
end
-- 2 hand-picked, lengths other than 1
local Picked = {
	{ Vector3.new(2, 0, 0), Vector3.new(0, 3, 0) },
	{ Vector3.new(0.5, 0, 0), Vector3.new(0, 0, -4) },
	{ Vector3.new(1, 1, 0), Vector3.new(0, 0, 1) },
	{ Vector3.new(1, 1, 1), Vector3.new(-1, 1, 1) },
	{ Vector3.new(1, 2, 3), Vector3.new(-4, 5, 0.5) },
	{ Vector3.new(0.3, -0.2, 0.9), Vector3.new(0.1, 0.7, -0.4) },
	{ Vector3.new(3, 4, 0), Vector3.new(0, 0, 5) },
	{ Vector3.new(1, 0, 0), Vector3.new(1, 1, 0) },
	{ Vector3.new(1, 0, 0), Vector3.new(-1, 1, 0) },
	{ Vector3.new(1, 0, 0), Vector3.new(-1, 0.001, 0) },
}
for Index, Pair in Picked do
	Line("picked " .. Index, Pair[1], Pair[2])
end
local Rng = P.rng(1207)
local function RandomVector(Size)
	return Vector3.new(Rng.real(-Size, Size), Rng.real(-Size, Size), Rng.real(-Size, Size))
end
-- 3 random pairs
for Case = 1, 40 do
	Line("random " .. Case, RandomVector(10), RandomVector(10))
end
-- 4 random pairs scaled by powers of two, to lengths whose squares (and their product) leave the float range
for _, Power in { -66, -40, -30, -10, 10, 30, 40, 66 } do
	for Case = 1, 3 do
		local A, B = RandomVector(10) * 2 ^ Power, RandomVector(10)
		Line("scaled " .. Power .. " " .. Case, A, B)
		Line("scaled-both " .. Power .. " " .. Case, A, B * 2 ^ Power)
	end
end
-- 5 exactly opposite: the half turn's axis for each largest component, ties included
local Opposite = {
	Vector3.new(1, 1, 0), Vector3.new(1, -1, 5), Vector3.new(3, 4, 0), Vector3.new(0, 1, 1), Vector3.new(1, 0, 1),
	Vector3.new(-2, 1, -2), Vector3.new(1, 2, 3), Vector3.new(-3, 2, 1), Vector3.new(0.25, -0.25, 0.25), Vector3.new(0, 0, 2),
}
for Index, A in Opposite do
	Line("opposite " .. Index, A, -A)
	Line("opposite-longer " .. Index, A, A * -3)
end
for Case = 1, 16 do
	local A = RandomVector(10)
	Line("opposite-random " .. Case, A, A * -Rng.pick({ 1, 2, 0.5, 0.125 }))
end
-- 6 nearly opposite: b is -a turned by Phi about an axis across a, Phi stepping through the cutoff, at three lengths, with
-- a's length apart from b's, and along a direction that is not an axis
local function Turned(Vector, Axis, Phi)
	-- Rodrigues' rotation, in doubles
	local U = Axis.Unit
	local C, S = math.cos(Phi), math.sin(Phi)
	local Ux, Uy, Uz = U.X, U.Y, U.Z
	local X, Y, Z = Vector.X, Vector.Y, Vector.Z
	local Dot = Ux * X + Uy * Y + Uz * Z
	return Vector3.new(
		X * C + (Uy * Z - Uz * Y) * S + Ux * Dot * (1 - C),
		Y * C + (Uz * X - Ux * Z) * S + Uy * Dot * (1 - C),
		Z * C + (Ux * Y - Uy * X) * S + Uz * Dot * (1 - C)
	)
end
local Cutoffs = {
	{ "x", Vector3.new(1, 0, 0), Vector3.new(0, 0, 1), 1 },
	{ "x-small", Vector3.new(2 ^ -8, 0, 0), Vector3.new(0, 0, 1), 1 },
	{ "x-large", Vector3.new(1024, 0, 0), Vector3.new(0, 0, 1), 1 },
	{ "x-apart", Vector3.new(1, 0, 0), Vector3.new(0, 0, 1), 1000 },
	{ "slant", Vector3.new(2, -3, 6), Vector3.new(3, 2, 0), 1 },
	{ "slant-large", Vector3.new(2, -3, 6) * 512, Vector3.new(3, 2, 0), 1 },
}
for _, Case in Cutoffs do
	local Label, A, Axis, Scale = Case[1], Case[2], Case[3], Case[4]
	for Step = 0, 40 do
		local Phi = 0.0040 + Step * 0.000025
		Line("cutoff " .. Label .. " " .. P.num(Phi), A, Turned(-A * Scale, Axis, Phi))
	end
end
-- 7 nearly opposite, coarsely, with b off the plane of a and its axis: whose perpendicular the half turn takes
for _, Phi in { 1e-7, 1e-6, 1e-5, 1e-4, 1e-3, 3e-3, 6e-3, 1e-2, 0.1 } do
	Line("near x-z " .. P.num(Phi), Vector3.new(1, 0, 0), Turned(Vector3.new(-1, 0, 0), Vector3.new(0, 1, 0), Phi))
	Line("near x-yz " .. P.num(Phi), Vector3.new(1, 0, 0), Turned(Vector3.new(-1, 0, 0), Vector3.new(0, 1, 1), Phi))
	Line("near y-x " .. P.num(Phi), Vector3.new(0, 1, 0), Turned(Vector3.new(0, -1, 0), Vector3.new(0, 0, 1), Phi))
	Line("near slant " .. P.num(Phi), Vector3.new(1, 2, 3), Turned(Vector3.new(-1, -2, -3), Vector3.new(1, 1, -1), Phi))
	Line("near tie " .. P.num(Phi), Vector3.new(1, 1, 0), Turned(Vector3.new(-1, -1, 0), Vector3.new(0, 0, 1), Phi))
end
-- 8 nearly and exactly equal
Line("equal 1", Vector3.new(5, -1, 2), Vector3.new(5, -1, 2))
Line("equal 2", Vector3.new(5, -1, 2), Vector3.new(5, -1, 2.001))
Line("equal 3", Vector3.new(0.1, 0.2, 0.3), Vector3.new(0.2, 0.4, 0.6))
for Case = 1, 8 do
	local A = RandomVector(10)
	Line("equal-random " .. Case, A, A + RandomVector(1e-3))
end
-- 9 zero, infinite and not-a-number vectors
local Nan = 0 / 0
Line("zero a", Vector3.zero, Vector3.new(1, 0, 0))
Line("zero b", Vector3.new(1, 0, 0), Vector3.zero)
Line("zero both", Vector3.zero, Vector3.zero)
Line("inf", Vector3.new(math.huge, 0, 0), Vector3.new(0, 1, 0))
Line("nan", Vector3.new(Nan, 0, 0), Vector3.new(0, 1, 0))
Line("tiny", Vector3.new(2 ^ -70, 0, 0), Vector3.new(0, 2 ^ -70, 0))
-- 10 arguments
print("args none " .. P.try(function()
	return CFrame.fromRotationBetweenVectors()
end))
print("args one " .. P.try(function()
	return CFrame.fromRotationBetweenVectors(Vector3.xAxis)
end))
print("args string " .. P.try(function()
	return CFrame.fromRotationBetweenVectors("1", Vector3.xAxis)
end))
print("args nil " .. P.try(function()
	return CFrame.fromRotationBetweenVectors(Vector3.xAxis, nil)
end))
print("args vector2 " .. P.try(function()
	return CFrame.fromRotationBetweenVectors(Vector3.xAxis, Vector2.new(1, 2))
end))
print("args extra " .. P.try(function()
	return CFrame.fromRotationBetweenVectors(Vector3.xAxis, Vector3.yAxis, 5)
end))
