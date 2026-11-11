-- Path2D without a layout: the control point list and its errors, the spline behind it, Path2DControlPoint, the
-- properties, ControlPointChanged, and the curve queries on offset-only points under a parent of no size (their
-- answers do not depend on a screen).
local Point = Path2DControlPoint.new
local Offset = UDim2.fromOffset
local function Show(Value)
	local Kind = typeof(Value)
	if Kind == "number" then
		return P.f32(Value)
	elseif Kind == "UDim2" then
		return "UDim2(" .. P.f32s(Value.X.Scale, Value.X.Offset, Value.Y.Scale, Value.Y.Offset) .. ")"
	elseif Kind == "Vector2" then
		return "Vector2(" .. P.f32s(Value.X, Value.Y) .. ")"
	elseif Kind == "Rect" then
		return "Rect(" .. P.f32s(Value.Min.X, Value.Min.Y, Value.Max.X, Value.Max.Y) .. ")"
	elseif Kind == "Path2DControlPoint" then
		return "Point(" .. Show(Value.Position) .. " " .. Show(Value.LeftTangent) .. " " .. Show(Value.RightTangent) .. ")"
	elseif Kind == "table" then
		local Parts = {}
		for Index, Entry in Value do
			Parts[Index] = Show(Entry)
		end
		return "{" .. table.concat(Parts, ", ") .. "}"
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
local function Positions(Path)
	local Parts = {}
	for Index, Entry in Path:GetControlPoints() do
		Parts[Index] = Entry.Position.X.Offset .. "," .. Entry.Position.Y.Offset
	end
	return table.concat(Parts, ";")
end
-- (parents outside the DataModel, each kept in this list to the probe's end)
local Kept = {}
local function Parent(Class)
	local Holder = Instance.new(Class)
	table.insert(Kept, Holder)
	return Holder
end
P.sync()

-- the class
local Path = Instance.new("Path2D")
for _, Name in { "Closed", "Color3", "Thickness", "Visible", "ZIndex", "Transparency", "SelectedControlPoint", "PropertiesSerialize", "AbsoluteSize" } do
	Try("read " .. Name, function() return (Path :: any)[Name] end)
end
Try("write Transparency", function() (Path :: any).Transparency = 0.5 end)
print("isa", Path:IsA("GuiBase"), Path:IsA("GuiBase2d"), Path:IsA("GuiObject"))
Try("max", function() return Path:GetMaxControlPoints() end)
Try("segment count", function() return (Path :: any):GetSegmentCount() end)
for _, Case in { { "thickness", "Thickness", { -3, 0, 2.5, 100, 100.5, 1e6 } }, { "zindex", "ZIndex", { 2.7, -5, 2.5, 3.5 } } } do
	local Parts = {}
	for Index, Value in Case[3] do
		(Path :: any)[Case[2]] = Value
		Parts[Index] = P.f32((Path :: any)[Case[2]])
	end
	print(Case[1], table.concat(Parts, " "))
end

-- Path2DControlPoint
Try("point()", function() return Point() end)
Try("point(p)", function() return Point(UDim2.new(0.25, 7, 0.5, -9)) end)
Try("point(p, l)", function() return (Point :: any)(Offset(1, 2), Offset(3, 4)) end)
Try("point(p, l, r)", function() return Point(UDim2.new(0.25, 7, 0.5, -9), UDim2.new(0.125, 1, 0.0625, 2), UDim2.new(-0.5, 3, 2, 4)) end)
Try("point(p, l, r, x)", function() return (Point :: any)(Offset(1, 2), Offset(3, 4), Offset(5, 6), 7) end)
Try("point(nil, l, r)", function() return (Point :: any)(nil, Offset(3, 4), Offset(5, 6)) end)
Try("point(1)", function() return (Point :: any)(1) end)
Try("point(p, 1, r)", function() return (Point :: any)(Offset(1, 2), 1, Offset(5, 6)) end)
Try("point(p, l, 'r')", function() return (Point :: any)(Offset(1, 2), Offset(3, 4), "r") end)
Try("point(p, 1)", function() return (Point :: any)(Offset(1, 2), 1) end)
Try("point(nil)", function() return (Point :: any)(nil) end)
Try("point fractional offset", function() return Point(Offset(1.5, -1.5), Offset(0.4, 0.6), Offset(2.5, -2.5)) end)
print("point typeof", typeof(Point()), type(Point()), tostring(Point(Offset(1, 2), Offset(3, 4), Offset(5, 6))))
print("point eq", Point() == Point(), Point(Offset(1, 2)) == Point(Offset(1, 2)), Point(Offset(1, 2)) == Point(Offset(1, 3)), Point() == Point(Offset(0, 0), Offset(0, 0), Offset(0, 1)))
do
	local Value = Point(Offset(1, 2))
	Try("point write", function() Value.Position = Offset(9, 9) Value.LeftTangent = Offset(-1, 0) Value.RightTangent = Offset(1, 0) return Value end)
	Try("point write number", function() (Value :: any).Position = 5 end)
	Try("point write nil", function() (Value :: any).LeftTangent = nil end)
	Try("point write unknown", function() (Value :: any).Foo = 5 end)
	Try("point read unknown", function() return (Value :: any).Foo end)
	Try("point read lower", function() return (Value :: any).position end)
	Try("point call", function() return (Value :: any):Foo() end)
	-- a point in a path is a copy
	local Holder = Instance.new("Path2D")
	Holder:SetControlPoints({ Value })
	Value.Position = Offset(50, 50)
	local Read = Holder:GetControlPoint(1)
	Read.Position = Offset(70, 70)
	print("point copies", Positions(Holder))
end

-- the list: arguments and errors (no parent)
Try("set number", function() (Path :: any):SetControlPoints(5) end)
Try("set nil", function() (Path :: any):SetControlPoints(nil) end)
Try("set none", function() (Path :: any):SetControlPoints() end)
Try("set string", function() (Path :: any):SetControlPoints("a") end)
Try("set bad entry", function() (Path :: any):SetControlPoints({ Point(), 5 }) end)
Try("set bad first", function() (Path :: any):SetControlPoints({ "a", Point() }) end)
Try("set dictionary", function() (Path :: any):SetControlPoints({ a = Point() }) end)
Try("set mixed", function() (Path :: any):SetControlPoints({ Point(Offset(1, 1)), a = Point() }) end)
Try("set hole", function() (Path :: any):SetControlPoints({ Point(Offset(1, 1)), nil, Point(Offset(3, 3)) }) end)
print("after bad sets", #Path:GetControlPoints(), Positions(Path))
Try("get empty", function() return Path:GetControlPoints() end)
for _, Index in { 0, 1 } do
	Try("get " .. Index .. " empty", function() return Path:GetControlPoint(Index) end)
	Try("remove " .. Index .. " empty", function() return Path:RemoveControlPoint(Index) end)
	Try("update " .. Index .. " empty", function() return Path:UpdateControlPoint(Index, Point()) end)
end
Try("insert 2 empty", function() return Path:InsertControlPoint(2, Point()) end)
Try("insert 1 empty", function() return Path:InsertControlPoint(1, Point()) end)
Path:SetControlPoints({ Point(Offset(10, 0)), Point(Offset(20, 0)), Point(Offset(30, 0)) })
for _, Index in { 0, 1, 3, 4, -1, 1.4, 1.5, 2.49, 2.5, 0.4, 0.5, 3.4, 3.5, "2", " 3 ", "x", true, math.huge, 0 / 0, 2 ^ 31, -2 ^ 31 - 1 } do
	Try("get " .. tostring(Index), function() return (Path :: any):GetControlPoint(Index).Position.X.Offset end)
end
Try("get none", function() return (Path :: any):GetControlPoint() end)
Try("insert bad point", function() (Path :: any):InsertControlPoint(1, 5) end)
Try("insert no point", function() (Path :: any):InsertControlPoint(1) end)
Try("insert bad index", function() (Path :: any):InsertControlPoint("x", Point()) end)
Try("insert 0", function() Path:InsertControlPoint(0, Point()) end)
Try("insert 5", function() Path:InsertControlPoint(5, Point()) end)
Try("insert 4", function() Path:InsertControlPoint(4, Point()) end)
Try("update bad point", function() (Path :: any):UpdateControlPoint(1, "p") end)
Try("update no point", function() (Path :: any):UpdateControlPoint(1) end)
Try("update 0", function() Path:UpdateControlPoint(0, Point()) end)
Try("update 4", function() Path:UpdateControlPoint(4, Point()) end)
Try("update 2", function() Path:UpdateControlPoint(2, Point()) end)
Try("remove none", function() (Path :: any):RemoveControlPoint() end)
Try("remove 0", function() Path:RemoveControlPoint(0) end)
Try("remove 4", function() Path:RemoveControlPoint(4) end)
Try("remove 2", function() Path:RemoveControlPoint(2) end)
print("no parent list", Positions(Path))
do
	local Returned = Path:GetControlPoints()
	Returned[1] = nil
	print("returned is a copy", #Path:GetControlPoints())
end

-- the limit
do
	local Limit = Instance.new("Path2D")
	local Many = table.create(101, Point(Offset(1, 1)))
	Try("set 101", function() Limit:SetControlPoints(Many) end)
	print("after 101", #Limit:GetControlPoints())
	Many[101] = nil
	Try("set 100", function() Limit:SetControlPoints(Many) end)
	Try("insert at 100", function() Limit:InsertControlPoint(1, Point()) end)
	Try("insert 0 at 100", function() Limit:InsertControlPoint(0, Point()) end)
	Try("insert 102 at 100", function() Limit:InsertControlPoint(102, Point()) end)
	Try("insert 101 at 100", function() Limit:InsertControlPoint(101, Point()) end)
	print("after limit", #Limit:GetControlPoints())
end

-- queries without a usable parent
do
	local Loose = Instance.new("Path2D")
	local Queries = {
		{ "length", function(Target) return Target:GetLength() end },
		{ "rect", function(Target) return Target:GetBoundingRect() end },
		{ "position", function(Target) return Target:GetPositionOnCurve(0.5) end },
		{ "tangent", function(Target) return Target:GetTangentOnCurve(0.5) end },
		{ "position arc", function(Target) return Target:GetPositionOnCurveArcLength(0.5) end },
		{ "tangent arc", function(Target) return Target:GetTangentOnCurveArcLength(0.5) end },
	}
	local function All(Label, Target)
		for _, Query in Queries do
			Try(Label .. " " .. Query[1], function() return Query[2](Target) end)
		end
	end
	All("no points, no parent", Loose)
	Loose:SetControlPoints({ Point(Offset(3, 4)) })
	All("one point, no parent", Loose)
	Loose:SetControlPoints({ Point(Offset(3, 4)), Point(Offset(13, 4)) })
	All("two points, no parent", Loose)
	Try("position no argument", function() return (Loose :: any):GetPositionOnCurve() end)
	Try("position string", function() return (Loose :: any):GetPositionOnCurve("x") end)
	for _, Class in { "Folder", "Path2D", "UICorner", "UIListLayout", "Part" } do
		local Holder = Parent(Class)
		Loose.Parent = Holder
		Try("under " .. Class .. " length", function() return Loose:GetLength() end)
		Try("under " .. Class .. " insert", function() Loose:InsertControlPoint(1, Point()) end)
	end
	for _, Class in { "Frame", "TextLabel", "ScrollingFrame", "CanvasGroup", "ScreenGui", "SurfaceGui", "BillboardGui" } do
		local Holder = Parent(Class)
		Loose:SetControlPoints({ Point(Offset(3, 4)), Point(Offset(13, 4)) })
		Loose.Parent = Holder
		Try("under " .. Class .. " length", function() return Loose:GetLength() end)
		Try("under " .. Class .. " position", function() return Loose:GetPositionOnCurve(0.5) end)
	end
	local Holder = Parent("Frame")
	Loose.Parent = Holder
	Loose:SetControlPoints({})
	All("no points", Loose)
	Loose:SetControlPoints({ Point(Offset(3, 4)) })
	All("one point", Loose)
	Loose.Closed = true
	All("one point closed", Loose)
	Loose:SetControlPoints({ Point(Offset(3, 4)), Point(Offset(13, 4)) })
	All("two points closed", Loose)
	Try("two points closed end", function() return Loose:GetPositionOnCurve(0.75), Loose:GetTangentOnCurve(0.75) end)
	Loose.Closed = false
	Loose:SetControlPoints({ Point(Offset(3, 4)), Point(Offset(3, 4)) })
	All("coincident", Loose)
end

-- the spline behind the list: edits right after a set, after a query, after a frame
do
	local Holder = Parent("Frame")
	local Fresh = Instance.new("Path2D")
	Fresh.Parent = Holder
	Try("fresh insert 1", function() Fresh:InsertControlPoint(1, Point(Offset(1, 1))) end)
	Try("fresh insert 2", function() Fresh:InsertControlPoint(2, Point(Offset(2, 2))) end)
	Try("fresh update 1", function() Fresh:UpdateControlPoint(1, Point(Offset(5, 5))) end)
	Try("fresh remove 2", function() Fresh:RemoveControlPoint(2) end)
	print("fresh list", Positions(Fresh))
	local Edited = Instance.new("Path2D")
	Edited.Parent = Holder
	local Three = { Point(Offset(0, 0)), Point(Offset(10, 0)), Point(Offset(10, 10)) }
	Edited:SetControlPoints(Three)
	Try("set then insert 2", function() Edited:InsertControlPoint(2, Point(Offset(4, 4))) end)
	Try("set then insert 1", function() Edited:InsertControlPoint(1, Point(Offset(5, 5))) end)
	Try("set then update 1", function() Edited:UpdateControlPoint(1, Point(Offset(6, 6))) end)
	Try("set then update 3", function() Edited:UpdateControlPoint(3, Point(Offset(7, 7))) end)
	Try("set then remove 3", function() Edited:RemoveControlPoint(3) end)
	Try("set then remove 1", function() Edited:RemoveControlPoint(1) end)
	Try("set then remove 1 again", function() Edited:RemoveControlPoint(1) end)
	print("edited list", Positions(Edited))
	Edited:SetControlPoints(Three)
	Try("query length", function() return Edited:GetLength() end)
	Try("query then insert 4", function() Edited:InsertControlPoint(4, Point(Offset(4, 4))) end)
	Edited:SetControlPoints(Three)
	Try("query rect", function() return Edited:GetBoundingRect() end)
	Try("rect then insert 4", function() Edited:InsertControlPoint(4, Point(Offset(4, 4))) end)
	Edited:SetControlPoints(Three)
	Try("query position", function() return Edited:GetPositionOnCurve(0) end)
	Try("position then insert 4", function() Edited:InsertControlPoint(4, Point(Offset(4, 4))) end)
	Edited:SetControlPoints({})
	Try("set none then insert 1", function() Edited:InsertControlPoint(1, Point(Offset(4, 4))) end)
	Edited:SetControlPoints(Three)
	Try("stale insert 2", function() Edited:InsertControlPoint(2, Point(Offset(8, 8))) end)
	Try("stale insert 3", function() Edited:InsertControlPoint(3, Point(Offset(9, 9))) end)
	print("stale list", Positions(Edited))
	Edited:SetControlPoints(Three)
	task.wait()
	task.wait()
	Try("frame then insert 4", function() Edited:InsertControlPoint(4, Point(Offset(4, 4))) end)
	local Loose = Instance.new("Path2D")
	Loose:SetControlPoints(Three)
	task.wait()
	task.wait()
	Try("no parent, frame then remove 3", function() Loose:RemoveControlPoint(3) end)
	Loose.Parent = Holder
	Try("parented then insert 1", function() Loose:InsertControlPoint(1, Point(Offset(4, 4))) end)
	Try("parented then insert 3", function() Loose:InsertControlPoint(3, Point(Offset(4, 4))) end)
	task.wait()
	task.wait()
	Try("parented, frame then insert 3", function() Loose:InsertControlPoint(3, Point(Offset(4, 4))) end)
	local Copy = Edited:Clone()
	print("clone", Positions(Copy), Copy.Parent)
	Copy.Parent = Holder
	Try("clone insert 5", function() Copy:InsertControlPoint(5, Point(Offset(4, 4))) end)
	Try("clone insert 1", function() Copy:InsertControlPoint(1, Point(Offset(4, 4))) end)
end

-- signals
do
	local Holder = Parent("Frame")
	local Watched = Instance.new("Path2D")
	Watched.Parent = Holder
	local Log = {}
	Watched.ControlPointChanged:Connect(function(...)
		table.insert(Log, "ControlPointChanged(" .. select("#", ...) .. ")")
	end)
	Watched.Changed:Connect(function(Name)
		table.insert(Log, "Changed(" .. tostring(Name) .. ")")
	end)
	local function Flush(Label)
		print(Label, "now:", table.concat(Log, " "))
		table.clear(Log)
		task.wait()
		task.wait()
		print(Label, "later:", table.concat(Log, " "))
		table.clear(Log)
	end
	Watched:SetControlPoints({ Point(Offset(0, 0)), Point(Offset(10, 0)) })
	Flush("set")
	Watched:SetControlPoints({ Point(Offset(0, 0)), Point(Offset(10, 0)) })
	Flush("set same")
	Watched:GetLength() -- (the spline now holds the two points)
	Watched:InsertControlPoint(3, Point(Offset(10, 10)))
	Watched:UpdateControlPoint(1, Point(Offset(1, 1)))
	Watched:UpdateControlPoint(1, Point(Offset(1, 1)))
	Watched:RemoveControlPoint(2)
	Flush("edits")
	pcall(function() (Watched :: any):SetControlPoints({ 5 }) end)
	pcall(function() Watched:InsertControlPoint(9, Point()) end)
	pcall(function() Watched:RemoveControlPoint(9) end)
	Flush("failed")
	Watched:SetControlPoints({ Point(Offset(0, 0)), Point(Offset(10, 0)), Point(Offset(20, 0)), Point(Offset(30, 0)) })
	print("spline error", P.try(function() Watched:InsertControlPoint(5, Point()) end), #Watched:GetControlPoints())
	Flush("spline error")
	Watched:SetControlPoints({})
	Watched:SetControlPoints({})
	Flush("set empty twice")
	Watched.Thickness = 5
	Watched.Thickness = 5
	Watched.Thickness = 500
	Watched.Thickness = 600
	Watched.Closed = true
	Watched.Color3 = Color3.new(1, 0, 0)
	Watched.Visible = false
	Watched.ZIndex = 4
	Watched.Name = "Renamed"
	Flush("properties")
	local Seen = 0
	Watched:GetPropertyChangedSignal("Thickness"):Connect(function() Seen += 1 end)
	Watched.Thickness = 2
	task.wait()
	print("property signal", Seen)
	Try("property signal ControlPoints", function() return Watched:GetPropertyChangedSignal("ControlPoints") end)
end

-- the curve queries in pixels: offset-only points under a parent of no size
do
	local Holder = Parent("Frame")
	local Curve = Instance.new("Path2D")
	Curve.Parent = Holder
	local function Queries(Label, Times)
		Try(Label .. " length", function() return Curve:GetLength() end)
		Try(Label .. " rect", function() return Curve:GetBoundingRect() end)
		for _, Time in Times do
			Try(Label .. " t=" .. P.num(Time), function()
				return Curve:GetPositionOnCurve(Time), Curve:GetTangentOnCurve(Time)
			end)
		end
	end
	local function Arc(Label, Times)
		for _, Time in Times do
			Try(Label .. " arc t=" .. P.num(Time), function()
				return Curve:GetPositionOnCurveArcLength(Time), Curve:GetTangentOnCurveArcLength(Time)
			end)
		end
	end
	local Times = { 0, 0.125, 0.25, 0.5, 0.625, 0.75, 1 }
	Curve:SetControlPoints({ Point(Offset(0, 0)), Point(Offset(100, 0)), Point(Offset(100, 50)) })
	Queries("lines", Times)
	Queries("lines outside", { -0.5, 1.5, math.huge, -math.huge, 0 / 0 })
	Arc("lines", { 0, 0.125, 0.25, 0.5, 0.75, 0.875, 1, 1.5, -1 })
	Curve.Thickness = 20
	Try("lines thick rect", function() return Curve:GetBoundingRect(), Curve:GetLength() end)
	Curve.Thickness = 0
	Try("lines thin rect", function() return Curve:GetBoundingRect() end)
	Curve.Thickness = 1
	Curve.Closed = true
	Queries("lines closed", { 0, 0.25, 0.5, 2 / 3, 0.75, 1 })
	Arc("lines closed", { 0.25, 0.5, 0.75 })
	Curve.Closed = false
	-- one handle: quadratic; two: cubic; handles of the outer sides are not used
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 0), Offset(0, 60)), Point(Offset(100, 0)) })
	Queries("right handle", Times)
	Curve:SetControlPoints({ Point(Offset(0, 0)), Point(Offset(100, 0), Offset(0, 60), Offset(0, 0)) })
	Queries("left handle", Times)
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 60), Offset(0, 0)), Point(Offset(100, 0), Offset(0, 0), Offset(0, 60)) })
	Queries("outer handles", { 0, 0.25, 1 })
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 0), Offset(0, 55)), Point(Offset(100, 100), Offset(-55, 0), Offset(0, 0)) })
	Queries("arc", Times)
	-- (by length a client walks a curved segment in 32 steps and answers where the walk ends: short of the end
	-- here, and anywhere near a tight bend)
	Arc("arc", { 0, 0.25, 0.5, 0.75, 1 })
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 0), Offset(100, 0)), Point(Offset(100, 80), Offset(-100, 0), Offset(0, 0)) })
	Queries("s curve", Times)
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 0), Offset(300, 200)), Point(Offset(100, 0), Offset(-300, 200), Offset(0, 0)) })
	Queries("loop", Times)
	Arc("loop", { 0.25, 0.5, 0.75 })
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(-30, 0), Offset(0, 60)), Point(Offset(100, 50), Offset(-50, 0), Offset(25, 50)), Point(Offset(180, 10), Offset(0, 40), Offset(20, -20)) })
	Queries("three points", { 0, 0.1, 0.25, 0.4, 0.5, 0.6, 0.75, 0.9, 1 })
	Curve.Closed = true
	Queries("three points closed", { 0, 0.1, 0.25, 1 / 3, 0.5, 2 / 3, 0.7, 0.9, 1 })
	Curve.Closed = false
	-- bounds: where the derivative has no square term, where it has a double root, where a handle leaves the ends
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 0), Offset(30, 90)), Point(Offset(90, 0), Offset(-30, 90), Offset(0, 0)) })
	Try("bounds symmetric", function() return Curve:GetBoundingRect() end)
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 0), Offset(30, 90)), Point(Offset(90, 0), Offset(-30, 91), Offset(0, 0)) })
	Try("bounds nearly symmetric", function() return Curve:GetBoundingRect() end)
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 0), Offset(-40, 10)), Point(Offset(90, 0), Offset(70, -30), Offset(0, 0)) })
	Try("bounds both axes", function() return Curve:GetBoundingRect() end)
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 0), Offset(50, -80)), Point(Offset(90, 0)) })
	Try("bounds quadratic", function() return Curve:GetBoundingRect() end)
	-- lengths
	local Generator = P.rng(7)
	for Index = 1, 16 do
		local Size = if Index <= 10 then 300 elseif Index <= 13 then 30 else 3000
		local function Pair(Low, High)
			return Offset(Generator.int(Low, High), Generator.int(Low, High))
		end
		Curve:SetControlPoints({ Point(Pair(0, Size), Offset(0, 0), Pair(-Size, Size)), Point(Pair(0, Size), if Index % 4 == 0 then Offset(0, 0) else Pair(-Size, Size), Offset(0, 0)) })
		Try("random " .. Index, function() return Curve:GetLength(), Curve:GetBoundingRect(), Curve:GetPositionOnCurve(0.3), Curve:GetTangentOnCurve(0.3) end)
	end
	-- by length: a quadratic; segments before the one that answers; a tight bend the walk runs past; out of range
	Curve:SetControlPoints({ Point(Offset(0, 0), Offset(0, 0), Offset(80, 140)), Point(Offset(220, 30)) })
	Arc("quadratic", { 0.2, 0.425, 0.9 })
	Curve:SetControlPoints({ Point(Offset(0, 0)), Point(Offset(130, 20), Offset(0, 0), Offset(40, 90)), Point(Offset(300, 70), Offset(-90, -10), Offset(0, 0)), Point(Offset(310, 200)) })
	Arc("mixed", { 0.1, 0.325, 0.6, 0.825, 1 })
	Curve:SetControlPoints({ Point(Offset(197, 91), Offset(0, 0), Offset(108, 104)), Point(Offset(190, 299), Offset(-197, 230), Offset(0, 0)) })
	Try("tight length", function() return Curve:GetLength() end)
	Arc("tight", { 0.5, 0.75, 0.9, 1, 1.5, -1 })
	-- three straight segments whose float32 lengths leave the last a hair short at the end: the start answers
	Curve:SetControlPoints({ Point(Offset(0, 0)), Point(Offset(110, 80)), Point(Offset(200, 0)) })
	Arc("short of the end", { 0.5, 0.999, 1 })
	-- a curve that turns back on itself: the length to the last digit
	Curve:SetControlPoints({ Point(Offset(13, -16), Offset(0, 0), Offset(0, -36)), Point(Offset(13, -34)) })
	Try("turn back", function() return Curve:GetLength(), Curve:GetBoundingRect() end)
	-- fractional offsets are whole in a point; scale against no size
	Curve:SetControlPoints({ Point(Offset(0.6, 0.4)), Point(UDim2.new(0.5, 10, 2, 0)) })
	Try("fraction and scale", function() return Curve:GetControlPoints(), Curve:GetLength(), Curve:GetPositionOnCurve(1) end)
end
print("parents kept", #Kept, Kept[#Kept].ClassName, Kept[#Kept].Parent)
