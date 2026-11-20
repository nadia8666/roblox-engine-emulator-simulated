-- TweenService's Color3 blend on every Color3 property, classed per channel against the two forms clients have used, in
-- float32: `sq` sqrt(a^2 + (b^2 - a^2) * eased), 0 where that is below 0 (Studio 0.746), and `lin` a + (b - a) * eased
-- (0.747); Color3:Lerp stays linear. The formulas are computed here from a NumberValue tween's eased alpha, paused in the
-- same step, so the lines hold whatever the frame times were (a channel whose ends are equal reads `sq` under both).
local TweenService = game:GetService("TweenService")
local function F(X)
	return Vector3.new(X, 0, 0).X
end
local function Squared(A, B, T)
	local A2, B2, Tf = F(A * A), F(B * B), F(T)
	local X = F(A2 + F(F(B2 - A2) * Tf))
	return if X > 0 then F(math.sqrt(X)) else 0
end
local function Linear(A, B, T)
	return F(A + F(F(B - A) * F(T)))
end
local function Class(Value, From, To, T)
	local Out = {}
	for _, Channel in { "R", "G", "B" } do
		local V, A, B = Value[Channel], From[Channel], To[Channel]
		table.insert(Out, if V == Squared(A, B, T) then "sq" elseif V == Linear(A, B, T) then "lin" else "?" .. P.f32(V))
	end
	return table.concat(Out, ",")
end
local Pairs = {
	{ Color3.fromRGB(200, 40, 40), Color3.fromRGB(40, 40, 200) },
	{ Color3.new(0, 0, 0), Color3.new(1, 1, 1) },
	{ Color3.new(1, 1, 1), Color3.new(0, 0, 0) },
	{ Color3.new(-1, 2, 0.5), Color3.new(0.25, -0.5, 3) },
	{ Color3.new(0.3, 0.7, 0.123), Color3.new(0.3, 0.1, 0.9) },
}

-- every style and direction, paused early, half way and late (Back and Elastic take the eased alpha below 0 and past 1)
P.sync()
for _, Stop in { 0.15, 0.5, 0.85 } do
	local Groups, All = {}, {}
	for _, Style in Enum.EasingStyle:GetEnumItems() do
		if Style.Name == "Spring" or Style.Name == "Bezier" then
			continue
		end
		for _, Direction in Enum.EasingDirection:GetEnumItems() do
			local Info = TweenInfo.new(1, Style, Direction)
			local Eased = Instance.new("NumberValue")
			table.insert(All, TweenService:Create(Eased, Info, { Value = 1 }))
			local Values = {}
			for _, Pair in Pairs do
				local Value = Instance.new("Color3Value")
				Value.Value = Pair[1]
				table.insert(Values, Value)
				table.insert(All, TweenService:Create(Value, Info, { Value = Pair[2] }))
			end
			table.insert(Groups, { Name = Style.Name .. " " .. Direction.Name, Eased = Eased, Values = Values })
		end
	end
	for _, Tween in All do
		Tween:Play()
	end
	task.wait(Stop)
	for _, Tween in All do
		Tween:Pause()
	end
	for _, Group in Groups do
		local Out = {}
		for Index, Value in Group.Values do
			table.insert(Out, Class(Value.Value, Pairs[Index][1], Pairs[Index][2], Group.Eased.Value))
		end
		print("blend " .. Stop .. " " .. Group.Name .. " " .. table.concat(Out, " "))
	end
end

-- overshoot past an end, classed against the eased alpha read in the same step (the value itself depends on the frame
-- times): linear goes past the end, below 0 here; the squared blend reads exactly 0 where its mix goes below 0
local function Overshoot(Style, Direction, From, To)
	local Info = TweenInfo.new(1, Style, Direction)
	local Value, Eased = Instance.new("Color3Value"), Instance.new("NumberValue")
	Value.Value = From
	local Tweens = { TweenService:Create(Value, Info, { Value = To }), TweenService:Create(Eased, Info, { Value = 1 }) }
	for _, Tween in Tweens do
		Tween:Play()
	end
	repeat
		task.wait()
	until Eased.Value < -0.01 or Eased.Value > 1.01
	for _, Tween in Tweens do
		Tween:Pause()
	end
	local Read = Value.Value
	return Class(Read, From, To, Eased.Value) .. " " .. (if Read.R < 0 then "below-0" elseif Read.R == 0 then "0" else "above-0")
end
print("overshoot back-in " .. Overshoot(Enum.EasingStyle.Back, Enum.EasingDirection.In, Color3.new(0, 0, 0), Color3.new(1, 1, 1)))
print("overshoot back-out " .. Overshoot(Enum.EasingStyle.Back, Enum.EasingDirection.Out, Color3.new(1, 1, 1), Color3.new(0, 0, 0)))

-- every class's Color3 properties blend the same way
local A, B = Color3.fromRGB(200, 40, 40), Color3.fromRGB(40, 40, 200)
local Gui = Instance.new("ScreenGui")
local Frame = Instance.new("Frame", Gui)
local Label = Instance.new("TextLabel", Gui)
local Image = Instance.new("ImageLabel", Gui)
local Part = Instance.new("Part")
Part.Anchored = true
Part.Parent = workspace
local Targets = {
	{ Frame, "BackgroundColor3" }, { Frame, "BorderColor3" }, { Label, "TextColor3" }, { Label, "TextStrokeColor3" },
	{ Image, "ImageColor3" }, { Instance.new("UIStroke", Frame), "Color" }, { Instance.new("PointLight", Part), "Color" },
	{ Instance.new("Highlight", Part), "FillColor" }, { Instance.new("Color3Value"), "Value" },
	{ Instance.new("SelectionBox", Part), "Color3" }, { Instance.new("Texture", Part), "Color3" },
	{ Instance.new("Fire", Part), "Color" }, { Instance.new("Atmosphere"), "Color" },
}
local Raw = Instance.new("NumberValue")
local All = { TweenService:Create(Raw, TweenInfo.new(1, Enum.EasingStyle.Linear), { Value = 1 }) }
for _, Target in Targets do
	Target[1][Target[2]] = A
	table.insert(All, TweenService:Create(Target[1], TweenInfo.new(1, Enum.EasingStyle.Linear), { [Target[2]] = B }))
end
for _, Tween in All do
	Tween:Play()
end
task.wait(0.4)
for _, Tween in All do
	Tween:Pause()
end
for _, Target in Targets do
	print("class " .. Target[1].ClassName .. "." .. Target[2] .. " " .. Class(Target[1][Target[2]], A, B, Raw.Value))
end
-- a part keeps its colour in whole 1/255 steps: the blend, cut down to one
local PartTween = TweenService:Create(Part, TweenInfo.new(1, Enum.EasingStyle.Linear), { Color = B })
local PartRaw = Instance.new("NumberValue")
local PartRawTween = TweenService:Create(PartRaw, TweenInfo.new(1, Enum.EasingStyle.Linear), { Value = 1 })
Part.Color = A
PartTween:Play()
PartRawTween:Play()
task.wait(0.4)
PartTween:Pause()
PartRawTween:Pause()
local Steps = {}
for _, Channel in { "R", "G", "B" } do
	local Read, Label = Part.Color[Channel], nil
	for _, Form in { { "lin", Linear(A[Channel], B[Channel], PartRaw.Value) }, { "sq", Squared(A[Channel], B[Channel], PartRaw.Value) } } do
		local Name, Blend = Form[1], Form[2]
		Label = Label or (if Read == F(math.floor(Blend * 255) / 255) then "floor(" .. Name .. ")" elseif Read == F(math.floor(Blend * 255 + 0.5) / 255) then "round(" .. Name .. ")" elseif Read == Blend then Name else nil)
	end
	table.insert(Steps, Label or "?" .. P.f32(Read))
end
print("class Part.Color " .. table.concat(Steps, ","))

-- colour types TweenService does not tween
local Gradient = Instance.new("UIGradient", Frame)
print("untweened " .. P.try(TweenService.Create, TweenService, Gradient, TweenInfo.new(1), { Color = ColorSequence.new(B) })
	.. " | " .. P.try(TweenService.Create, TweenService, Part, TweenInfo.new(1), { BrickColor = BrickColor.new("Bright red") })
	.. " | " .. P.try(TweenService.Create, TweenService, Instance.new("BrickColorValue"), TweenInfo.new(1), { Value = BrickColor.new("Bright red") })
	.. " | " .. P.try(TweenService.Create, TweenService, Instance.new("Beam"), TweenInfo.new(1), { Color = ColorSequence.new(B) }))

-- the end is the blend at 1 (not the goal itself), and a reversed tween ends on the blend at 0 (a channel's sign is lost)
local Ending = Instance.new("Color3Value")
Ending.Value = A
local Tween = TweenService:Create(Ending, TweenInfo.new(0.1), { Value = B })
Tween:Play()
Tween.Completed:Wait()
print("end " .. P.f32s(Ending.Value.R, Ending.Value.G, Ending.Value.B) .. " goal " .. P.f32s(B.R, B.G, B.B))
local Back = Instance.new("Color3Value")
Back.Value = Color3.new(-0.5, 0.3, 2)
Tween = TweenService:Create(Back, TweenInfo.new(0.1, Enum.EasingStyle.Linear, Enum.EasingDirection.In, 0, true), { Value = Color3.new(0.2, -0.7, 0.1) })
Tween:Play()
Tween.Completed:Wait()
print("reversed-end " .. P.f32s(Back.Value.R, Back.Value.G, Back.Value.B))
print("lerp " .. P.f32s(A:Lerp(B, 0.5).R, A:Lerp(B, 0.5).G, A:Lerp(B, 0.5).B))
Part:Destroy()
