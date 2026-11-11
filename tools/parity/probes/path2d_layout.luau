-- parity: device=desktop rendered
-- Path2D against a layout: how control points resolve against the parent (scale and offset, a ScreenGui, a rotated
-- parent, a UIScale, a parent of negative size), what the curve queries answer in pixels, and when a resize reaches
-- them. Needs a GUI layout (a client with a screen, or a host that adds one).
local Players = game:GetService("Players")
local Point = Path2DControlPoint.new
local Offset = UDim2.fromOffset
-- (numbers to three decimals: this probe is about where a path is, and the last float32 digits of a turned or curved
-- path follow the engine's own arithmetic, which path2d.luau prints in full)
local function Numbers(...)
	local Parts = table.pack(...)
	for Index = 1, Parts.n do
		Parts[Index] = tostring(math.round(Parts[Index] * 1000) / 1000 + 0)
	end
	return table.concat(Parts, " ")
end
local function Show(Value)
	local Kind = typeof(Value)
	if Kind == "number" then
		return Numbers(Value)
	elseif Kind == "UDim2" then
		return "UDim2(" .. Numbers(Value.X.Scale, Value.X.Offset, Value.Y.Scale, Value.Y.Offset) .. ")"
	elseif Kind == "Vector2" then
		return "Vector2(" .. Numbers(Value.X, Value.Y) .. ")"
	elseif Kind == "Rect" then
		return "Rect(" .. Numbers(Value.Min.X, Value.Min.Y, Value.Max.X, Value.Max.Y) .. ")"
	end
	return Kind .. " " .. tostring(Value)
end
local function Try(Label, Callback)
	local Results = table.pack(pcall(Callback))
	if Results[1] then
		local Parts = {}
		for Index = 2, Results.n do
			Parts[Index - 1] = Show(Results[Index])
		end
		print(Label, "->", table.concat(Parts, " | "))
	else
		print(Label, "-> error", P.err(Results[2]))
	end
end
P.sync()
local Gui = Instance.new("ScreenGui")
Gui.Name = "ParityPath2D"
Gui.IgnoreGuiInset = true
Gui.ResetOnSpawn = false
local Frame = Instance.new("Frame")
Frame.Position = Offset(50, 30)
Frame.Size = Offset(200, 100)
Frame.Parent = Gui
Gui.Parent = Players.LocalPlayer:WaitForChild("PlayerGui")
task.wait()
print("screen", Show(Gui.AbsoluteSize), "frame", Show(Frame.AbsolutePosition), Show(Frame.AbsoluteSize))
local function Queries(Label, Path, Times)
	Try(Label .. " length", function() return Path:GetLength() end)
	Try(Label .. " rect", function() return Path:GetBoundingRect() end)
	for _, Time in Times do
		Try(Label .. " t=" .. P.num(Time), function()
			return Path:GetPositionOnCurve(Time), Path:GetTangentOnCurve(Time)
		end)
	end
end
local function New(Parent, Points)
	local Path = Instance.new("Path2D")
	Path:SetControlPoints(Points)
	Path.Parent = Parent
	return Path
end
local Times = { 0, 0.25, 0.5, 0.75, 1 }
-- scale and offset against the parent's size and position
local Scaled = { Point(UDim2.new(0, 0, 0, 0)), Point(UDim2.new(0.5, 10, 1, -20)), Point(UDim2.new(1, 0, 0, 0)) }
local Path = New(Frame, Scaled)
Queries("frame", Path, Times)
Try("frame arc", function() return Path:GetPositionOnCurveArcLength(0.25), Path:GetTangentOnCurveArcLength(0.25) end)
Path.Closed = true
Queries("frame closed", Path, Times)
Path.Closed = false
-- tangents resolve as sizes
local Handles = New(Frame, { Point(Offset(0, 0), Offset(-30, 0), Offset(0, 60)), Point(Offset(100, 50), UDim2.new(-0.25, 0, 0, 0), UDim2.new(0.1, 5, 0.5, 0)), Point(Offset(180, 10), Offset(0, 40), Offset(20, -20)) })
Queries("handles", Handles, { 0, 0.1, 0.25, 0.4, 0.5, 0.6, 0.75, 0.9, 1 })
-- a tangent whose scale and offset cancel is no handle
local Cancel = New(Frame, { Point(Offset(0, 0), Offset(0, 0), UDim2.new(0.1, -20, 0, 0)), Point(Offset(100, 0)) })
Queries("cancelled handle", Cancel, { 0, 0.2, 1 })
-- a resize reaches the queries with the next layout
Try("before resize", function() return Path:GetLength(), Path:GetPositionOnCurve(0.25) end)
Frame.Size = Offset(400, 100)
Try("resized, same step", function() return Path:GetLength(), Path:GetPositionOnCurve(0.25) end)
task.wait()
Try("resized, next frame", function() return Path:GetLength(), Path:GetPositionOnCurve(0.25) end)
Frame.Size = Offset(200, 100)
task.wait()
-- under the ScreenGui
local Screen = New(Gui, { Point(Offset(10, 20)), Point(UDim2.new(0.5, 0, 0.5, 0)), Point(UDim2.new(1, -10, 0, 20)) })
Queries("screen", Screen, { 0, 0.5, 1 })
-- a parent with no size on one axis answers in offsets
local Flat = Instance.new("Frame")
Flat.Position = Offset(10, 20)
Flat.Size = Offset(200, 0)
Flat.Parent = Gui
local InFlat = New(Flat, { Point(UDim2.new(0.5, 10, 0.5, 20)), Point(UDim2.new(1, 0, 1, 60)) })
task.wait()
Queries("flat parent", InFlat, { 0, 0.5, 1 })
Flat.Size = Offset(-50, 100)
task.wait()
print("negative size", Show(Flat.AbsoluteSize), Show(Flat.AbsolutePosition))
Queries("negative parent", InFlat, { 0, 0.5, 1 })
-- a rotated parent turns the path; a nested rotation too
local Turned = Instance.new("Frame")
Turned.Position = Offset(100, 200)
Turned.Size = Offset(100, 60)
Turned.Rotation = 30
Turned.Parent = Gui
local InTurned = New(Turned, { Point(Offset(0, 0), Offset(0, 0), Offset(40, 0)), Point(UDim2.new(1, 0, 1, 0)) })
local Inner = Instance.new("Frame")
Inner.Position = Offset(20, 10)
Inner.Size = Offset(40, 30)
Inner.Rotation = 45
Inner.Parent = Turned
local InInner = New(Inner, { Point(Offset(0, 0)), Point(UDim2.new(1, 0, 1, 0)) })
task.wait()
Queries("rotated", InTurned, { 0, 0.5, 1 })
Queries("rotated twice", InInner, { 0, 0.5, 1 })
-- a UIScale scales the parent, not the path's offsets or its thickness
Turned.Rotation = 0
Inner.Rotation = 0
local Scale = Instance.new("UIScale")
Scale.Scale = 2
Scale.Parent = Turned
task.wait()
print("scaled parent", Show(Turned.AbsoluteSize), Show(Turned.AbsolutePosition), "inner", Show(Inner.AbsoluteSize), Show(Inner.AbsolutePosition))
InTurned.Thickness = 10
Queries("under UIScale", InTurned, { 0, 0.5, 1 })
Queries("under UIScale nested", InInner, { 0, 1 })
-- the frame's layout brings the spline of a path on screen up to date with its points (off screen only a query does:
-- path2d.luau), also under a parent that is not shown and in a ScreenGui that is not enabled
local Three = { Point(Offset(0, 0)), Point(Offset(10, 0)), Point(Offset(10, 10)) }
local Off = Instance.new("ScreenGui")
Off.Enabled = false
Off.ResetOnSpawn = false
Off.Parent = Players.LocalPlayer.PlayerGui
local Unseen = Instance.new("Frame")
Unseen.Visible = false
Unseen.Size = Offset(50, 50)
Unseen.Parent = Gui
for _, Case in { { "on screen", Frame }, { "hidden parent", Unseen }, { "disabled gui", Off }, { "under the gui", Gui } } do
	local Shown = New(Case[2], {})
	Shown:SetControlPoints(Three)
	Try(Case[1] .. ": set then insert 4", function() Shown:InsertControlPoint(4, Point(Offset(4, 4))) end)
	Shown:SetControlPoints(Three)
	task.wait()
	task.wait()
	Try(Case[1] .. ": set, frames, insert 4", function() Shown:InsertControlPoint(4, Point(Offset(4, 4))) end)
	Shown.Visible = false
	Shown:SetControlPoints(Three)
	task.wait()
	task.wait()
	Try(Case[1] .. ": not visible, set, frames, insert 4", function() Shown:InsertControlPoint(4, Point(Offset(4, 4))) end)
	Shown:Destroy()
end
Off:Destroy()
-- a parent that is not shown still has its size
local Hidden = Instance.new("Frame")
Hidden.Visible = false
Hidden.Position = Offset(300, 300)
Hidden.Size = Offset(80, 40)
Hidden.Parent = Gui
local InHidden = New(Hidden, { Point(UDim2.new(0, 0, 0, 0)), Point(UDim2.new(1, 0, 1, 0)) })
task.wait()
Queries("hidden parent", InHidden, { 0.5 })
Gui:Destroy()
