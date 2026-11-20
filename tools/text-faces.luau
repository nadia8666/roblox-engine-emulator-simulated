-- Which font faces TextService:GetTextBoundsAsync measures as an Enum.Font face, run in a playing client by
-- tools/text-faces.mjs (which writes data/text-faces.json from what this prints). For every family an Enum.Font item
-- names, every FontWeight in both FontStyles: the bounds of two strings (a pangram, and printable ASCII) at sizes 14,
-- 20 and 48. Each Enum.Font face is measured first through GetTextBoundsAsync (which waits for the face to load) and
-- then through GetTextSize, whose answer is what ree's tables hold (an unloaded face would measure as SourceSans).
--   enum <Enum.Font name> | <bounds> | <GetTextSize>
--   face <family> <weight> <style> | <bounds>
local TextService = game:GetService("TextService")

local Texts = { "The quick brown fox jumps over the lazy dog" }
local Ascii = {}
for Code = 33, 126 do
	table.insert(Ascii, string.char(Code))
end
table.insert(Texts, table.concat(Ascii))
local Sizes = { 14, 20, 48 }

local function Bounds(Face)
	local Parts = {}
	for _, Text in Texts do
		for _, Size in Sizes do
			local Params = Instance.new("GetTextBoundsParams")
			Params.Text = Text
			Params.Font = Face
			Params.Size = Size
			local Ok, Result = pcall(TextService.GetTextBoundsAsync, TextService, Params)
			table.insert(Parts, if Ok then `{Result.X}x{Result.Y}` else "error")
		end
	end
	return table.concat(Parts, " ")
end

local Families, Order = {}, {}
for _, Item in Enum.Font:GetEnumItems() do
	local Ok, Face = pcall(Font.fromEnum, Item)
	if not Ok or Item.Name == "Unknown" then
		continue
	end
	local Measured = Bounds(Face)
	local Parts = {}
	for _, Text in Texts do
		for _, Size in Sizes do
			local Result = TextService:GetTextSize(Text, Size, Item, Vector2.new(1e6, 1e6))
			table.insert(Parts, `{Result.X}x{Result.Y}`)
		end
	end
	print(`enum {Item.Name} | {Measured} | {table.concat(Parts, " ")}`)
	if not Families[Face.Family] then
		Families[Face.Family] = true
		table.insert(Order, Face.Family)
	end
end
table.sort(Order)
for _, Family in Order do
	for _, Style in { Enum.FontStyle.Normal, Enum.FontStyle.Italic } do
		for _, Weight in Enum.FontWeight:GetEnumItems() do
			print(`face {Family} {Weight.Name} {Style.Name} | {Bounds(Font.new(Family, Weight, Style))}`)
		end
	end
end
