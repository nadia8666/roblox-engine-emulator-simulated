-- CFrame.new(x, y, z, qx, qy, qz, qw): unit quaternions (axis turns, half turns, random), the same negated, non-unit ones
-- (scaled by powers of ten up to where their squares leave the float range, random), zero, infinite and not-a-number
-- components. The inputs are rounded to floats first (through a Vector3), and each line prints them and the whole CFrame.
local function Float(Value)
	return Vector3.new(Value, 0, 0).X
end
local function Line(Label, X, Y, Z, Qx, Qy, Qz, Qw)
	X, Y, Z, Qx, Qy, Qz, Qw = Float(X), Float(Y), Float(Z), Float(Qx), Float(Qy), Float(Qz), Float(Qw)
	local Ok, Result = pcall(function()
		return CFrame.new(X, Y, Z, Qx, Qy, Qz, Qw)
	end)
	print(Label .. " | " .. P.nums(X, Y, Z, Qx, Qy, Qz, Qw) .. " | " .. (if Ok then P.nums(Result:GetComponents()) else "error " .. P.err(Result)))
end
local S, C = math.sin(math.pi / 4), math.cos(math.pi / 4)
-- 1 axis turns, half turns and the identity, each with its negation
local Unit = {
	{ "identity", 0, 0, 0, 1 }, { "x90", S, 0, 0, C }, { "y90", 0, S, 0, C }, { "z90", 0, 0, S, C },
	{ "x180", 1, 0, 0, 0 }, { "y180", 0, 1, 0, 0 }, { "z180", 0, 0, 1, 0 }, { "halves", 0.5, 0.5, 0.5, 0.5 },
	{ "halves-mixed", 0.5, -0.5, 0.5, -0.5 }, { "x60", 0.5, 0, 0, math.sqrt(3) / 2 }, { "x0.6", 0.6, 0, 0, 0.8 }, { "y0.6", 0, 0.6, 0, 0.8 },
}
for _, Q in Unit do
	Line(Q[1], 1, 2, 3, Q[2], Q[3], Q[4], Q[5])
	Line(Q[1] .. " negated", 1, 2, 3, -Q[2], -Q[3], -Q[4], -Q[5])
end
-- 2 random unit quaternions (made unit in doubles, then rounded to floats)
local Rng = P.rng(3102)
local function RandomUnit()
	local Qx, Qy, Qz, Qw = Rng.real(-1, 1), Rng.real(-1, 1), Rng.real(-1, 1), Rng.real(-1, 1)
	local Length = math.sqrt(Qx * Qx + Qy * Qy + Qz * Qz + Qw * Qw)
	return Qx / Length, Qy / Length, Qz / Length, Qw / Length
end
for Case = 1, 40 do
	local Qx, Qy, Qz, Qw = RandomUnit()
	Line("unit " .. Case, Rng.real(-100, 100), Rng.real(-100, 100), Rng.real(-100, 100), Qx, Qy, Qz, Qw)
end
-- 3 non-unit: hand-picked, random, and unit ones scaled up to where the squared length overflows or underflows a float
local Picked = { { 0, 0, 1, 1 }, { 1, 2, 3, 4 }, { 0, 0, 0, 2 }, { 0, 0, 0, 0.5 }, { -0.1, 0.7, 0.2, 0.6 }, { 1, 1, 1, 1 }, { 3, 0, 0, 4 }, { 0, -2, 0, -2 } }
for Index, Q in Picked do
	Line("picked " .. Index, 1, 2, 3, Q[1], Q[2], Q[3], Q[4])
end
for Case = 1, 30 do
	Line("nonunit " .. Case, 0, 0, 0, Rng.real(-2, 2), Rng.real(-2, 2), Rng.real(-2, 2), Rng.real(-2, 2))
end
for _, Scale in { 1e-30, 1e-23, 1e-20, 1e-10, 1e-3, 1 - 2 ^ -20, 1 + 2 ^ -20, 1e3, 1e10, 1e18, 1e19, 1e20, 1e30 } do
	for Case = 1, 2 do
		local Qx, Qy, Qz, Qw = RandomUnit()
		Line("scale " .. P.num(Scale) .. " " .. Case, 0, 0, 0, Qx * Scale, Qy * Scale, Qz * Scale, Qw * Scale)
	end
end
-- 4 zero, infinite and not-a-number components, and positions far out
Line("zero", 1, 2, 3, 0, 0, 0, 0)
Line("zero-negative", 1, 2, 3, -0, -0, -0, -0)
Line("inf", 1, 2, 3, math.huge, 0, 0, 1)
Line("nan", 1, 2, 3, 0 / 0, 0, 0, 1)
Line("far", 1e6, -2.5, 0.1, 0, 0, S, C)
Line("far-nonunit", -123456.78, 1e-7, 3e7, 1, 2, 3, 4)
