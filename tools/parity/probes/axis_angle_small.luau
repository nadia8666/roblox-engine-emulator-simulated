-- CFrame:ToAxisAngle near the identity and near a half turn, where the cosine of the angle (half the trace minus one)
-- is within a few float steps of 1 or -1: how finely the angle resolves there, and the axis it comes with.
local function Line(Label, C)
	local Axis, Theta = C:ToAxisAngle()
	print(Label .. " " .. P.f32s(Axis.X, Axis.Y, Axis.Z) .. " " .. P.num(Theta) .. " | " .. P.f32s(C:GetComponents()))
end
Line("identity", CFrame.identity)
Line("moved", CFrame.new(1, 2, 3))
-- one float step off the identity on the diagonal, and in the skew terms alone
Line("step-diagonal", CFrame.new(0, 0, 0, 0.99999994, 0, 0, 0, 1, 0, 0, 0, 1))
Line("step-diagonal3", CFrame.new(0, 0, 0, 0.99999994, 0, 0, 0, 0.99999994, 0, 0, 0, 0.99999994))
Line("skew-only", CFrame.new(0, 0, 0, 1, -0.0003, 0, 0.0003, 1, 0, 0, 0, 1))
local Small = { 1e-8, 1e-7, 1e-6, 1e-5, 5e-5, 1e-4, 2e-4, 3e-4, 4e-4, 4.8828125e-4, 5e-4, 6e-4, 7e-4, 8e-4, 1e-3, 1.5e-3, 2e-3, 3e-3, 5e-3, 1e-2, 2e-2, 5e-2, 0.1 }
for _, A in Small do
	Line("x " .. P.num(A), CFrame.Angles(A, 0, 0))
	Line("y " .. P.num(A), CFrame.Angles(0, A, 0))
	Line("z " .. P.num(-A), CFrame.Angles(0, 0, -A))
	Line("axis " .. P.num(A), CFrame.fromAxisAngle(Vector3.new(1, 2, -3), A))
	Line("pi-x " .. P.num(A), CFrame.Angles(math.pi - A, 0, 0))
	Line("pi-axis " .. P.num(A), CFrame.fromAxisAngle(Vector3.new(1, 2, -3), math.pi - A))
end
Line("pi", CFrame.Angles(0, math.pi, 0))
Line("pi-exact", CFrame.new(0, 0, 0, -1, 0, 0, 0, 1, 0, 0, 0, -1))
Line("pi-exact-xy", CFrame.new(0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, -1))
-- two frames that agree to rounding: the difference rotation a test would measure
local Rng = P.rng(19)
for Case = 1, 40 do
	local R = CFrame.fromEulerAnglesYXZ(Rng.real(-3, 3), Rng.real(-3, 3), Rng.real(-3, 3))
	local S = CFrame.fromEulerAnglesYXZ(Rng.real(-3, 3), Rng.real(-3, 3), Rng.real(-3, 3))
	Line("roundtrip " .. Case, (R * S):Inverse() * (R * S))
	Line("regroup " .. Case, ((R * S) * R):Inverse() * (R * (S * R)))
	Line("nudged " .. Case, R:Inverse() * (R * CFrame.Angles(0, Rng.real(0, 2e-3), 0)))
end
