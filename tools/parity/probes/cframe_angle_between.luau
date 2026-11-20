-- CFrame:AngleBetween(other): identical rotations (where rounding can put the cosine past 1), nearly identical and
-- nearly opposite ones, random pairs, positions (ignored), matrices that are not rotations (scaled, sheared, zero) and
-- not-a-number. Each line prints both rotations as stored and the angle, so the rule can be checked from the lines alone.
local function R(C)
	local _, _, _, R00, R01, R02, R10, R11, R12, R20, R21, R22 = C:GetComponents()
	return P.nums(R00, R01, R02, R10, R11, R12, R20, R21, R22)
end
local function Line(Label, A, B)
	local Ok, Result = pcall(function()
		return A:AngleBetween(B)
	end)
	print(Label .. " | " .. R(A) .. " | " .. R(B) .. " | " .. (if Ok then P.num(Result) else "error " .. P.err(Result)))
end
print("exists " .. P.try(function()
	return typeof(CFrame.identity.AngleBetween)
end))
local Rng = P.rng(4412)
local function RandomRotation()
	return CFrame.fromEulerAnglesYXZ(Rng.real(-3.2, 3.2), Rng.real(-3.2, 3.2), Rng.real(-3.2, 3.2))
end
-- 1 the identity, and turns about an axis by angles from a float step to past a half turn
Line("identity", CFrame.identity, CFrame.identity)
local Angles = { 1e-8, 1e-7, 3e-7, 1e-6, 1e-5, 1e-4, 3e-4, 1e-3, 1e-2, 0.1, 0.5, 1, 2, 3, math.pi - 1e-3, math.pi - 1e-5, math.pi, math.pi + 0.1, 5 }
for _, Angle in Angles do
	Line("x " .. P.num(Angle), CFrame.identity, CFrame.Angles(Angle, 0, 0))
	Line("axis " .. P.num(Angle), CFrame.identity, CFrame.fromAxisAngle(Vector3.new(1, 2, -3), Angle))
	Line("axis-back " .. P.num(Angle), CFrame.fromAxisAngle(Vector3.new(1, 2, -3), Angle), CFrame.identity)
end
-- 2 a rotation against itself: the cosine from the stored floats can round past 1
for Case = 1, 40 do
	local A = RandomRotation()
	Line("self " .. Case, A, A)
end
local Built = CFrame.fromAxisAngle(Vector3.new(1, 2, 3).Unit, 2.5)
Line("self built", Built, Built)
Line("self product", Built * Built * Built, Built * Built * Built)
-- 3 nearly identical: a rotation against itself turned a little more
for _, Angle in { 1e-9, 1e-8, 1e-7, 1e-6, 1e-5, 1e-4, 1e-3 } do
	for Case = 1, 4 do
		local A = RandomRotation()
		Line("near " .. P.num(Angle) .. " " .. Case, A, A * CFrame.Angles(0, Angle, 0))
	end
end
-- 4 nearly opposite
for _, Angle in { 1e-6, 1e-4, 1e-2 } do
	for Case = 1, 3 do
		local A = RandomRotation()
		Line("opposite " .. P.num(Angle) .. " " .. Case, A, A * CFrame.fromAxisAngle(Vector3.new(1, -1, 2), math.pi - Angle))
	end
end
-- 5 random pairs
for Case = 1, 60 do
	Line("random " .. Case, RandomRotation(), RandomRotation())
end
-- 6 positions do not count
Line("moved", CFrame.new(1, 2, 3) * CFrame.Angles(0.3, 1.1, -0.4), CFrame.new(-5, 0, 9) * CFrame.Angles(0.2, 1.1, -0.4))
-- 7 matrices that are not rotations
local function M(...)
	return CFrame.new(0, 0, 0, ...)
end
Line("scaled 2", M(2, 0, 0, 0, 2, 0, 0, 0, 2), CFrame.identity)
Line("scaled 0.5", M(0.5, 0, 0, 0, 0.5, 0, 0, 0, 0.5), CFrame.identity)
Line("scaled -1", M(-1, 0, 0, 0, -1, 0, 0, 0, -1), CFrame.identity)
Line("scaled -2", M(-2, 0, 0, 0, -2, 0, 0, 0, -2), CFrame.identity)
Line("zero", M(0, 0, 0, 0, 0, 0, 0, 0, 0), CFrame.identity)
Line("sheared", M(1, 0.5, 0, 0, 1, 0.25, 0, 0, 1), CFrame.Angles(0.1, 0.2, 0.3))
for Case = 1, 8 do
	local A = M(Rng.real(-2, 2), Rng.real(-2, 2), Rng.real(-2, 2), Rng.real(-2, 2), Rng.real(-2, 2), Rng.real(-2, 2), Rng.real(-2, 2), Rng.real(-2, 2), Rng.real(-2, 2))
	Line("matrix " .. Case, A, RandomRotation())
end
Line("nan", M(0 / 0, 0, 0, 0, 1, 0, 0, 0, 1), CFrame.identity)
-- 8 arguments
print("args none " .. P.try(function()
	return CFrame.identity:AngleBetween()
end))
print("args vector " .. P.try(function()
	return CFrame.identity:AngleBetween(Vector3.zero)
end))
print("args dot none " .. P.try(function()
	return CFrame.identity.AngleBetween()
end))
print("args extra " .. P.try(function()
	return CFrame.identity:AngleBetween(CFrame.identity, 1)
end))
print("args nil " .. P.try(function()
	return CFrame.identity:AngleBetween(nil)
end))
print("args dot self " .. P.try(function()
	return CFrame.identity.AngleBetween(5, CFrame.identity)
end))
print("args dot other " .. P.try(function()
	return CFrame.identity.AngleBetween(CFrame.identity, "x")
end))
local Method = CFrame.identity.AngleBetween
print("args stored " .. P.try(function()
	return Method(CFrame.identity, Vector3.zero)
end))
print("args stored ok " .. P.try(function()
	return Method(CFrame.identity, CFrame.Angles(0, 1, 0))
end))
print("args lowercase " .. P.try(function()
	return typeof(CFrame.identity.angleBetween)
end))
