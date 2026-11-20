-- TextService:GetTextBoundsAsync(GetTextBoundsParams): the size of a text in a Font face, as GetTextSize measures an
-- Enum.Font. Whether a call waits for its face to load (the first call of a face the client has not loaded yields; a
-- call that cannot yield, from a metamethod, raises), GetTextBoundsParams' defaults and how Size and Width store, every
-- Enum.Font face against GetTextSize, faces no Enum.Font names, sizes, widths that wrap, newlines, RichText, and the
-- errors. A server has no typesetting (--server). When a face finishes loading depends on the machine, so no line
-- prints the order of a yielding call against other threads.
local TextService = game:GetService("TextService")
local Families = "rbxasset://fonts/families/"

local function Params(Text, Face, Size, Width, Rich)
	local Made = Instance.new("GetTextBoundsParams")
	Made.Text = Text
	if Face then
		Made.Font = Face
	end
	if Size then
		Made.Size = Size
	end
	if Width then
		Made.Width = Width
	end
	if Rich ~= nil then
		Made.RichText = Rich
	end
	return Made
end

local function Show(Value)
	if typeof(Value) == "Vector2" then
		return `{P.f32(Value.X)} {P.f32(Value.Y)}`
	end
	return `{typeof(Value)} {tostring(Value)}`
end

local function Bounds(Made)
	local Ok, Result = pcall(function()
		return TextService:GetTextBoundsAsync(Made)
	end)
	return if Ok then Show(Result) else "error " .. P.err(Result)
end

-- a call from a metamethod, which cannot yield
local function FromMetamethod(Made)
	local Meta = setmetatable({}, {
		__index = function()
			return Show(TextService:GetTextBoundsAsync(Made))
		end,
	})
	local Ok, Result = pcall(function()
		return Meta.Bounds
	end)
	return if Ok then "ok " .. Result else "error " .. P.err(Result)
end

local function FaceText(Face)
	return `{string.gsub(Face.Family, "^" .. Families, "")} {Face.Weight.Name} {Face.Style.Name}`
end

-- Loading: the first call of a face waits for it, a second call does not, a face that resolves to a loaded one does not
do
	local Face = Font.fromEnum(Enum.Font.Bangers)
	local First = "pending"
	local Thread = coroutine.create(function()
		First = Bounds(Params("Hello", Face, 20))
	end)
	coroutine.resume(Thread)
	print("first call of a face:", coroutine.status(Thread), First)
	for _ = 1, 600 do
		if First ~= "pending" then
			break
		end
		task.wait()
	end
	print("first call returned:", First)
	local Second = "pending"
	local Again = coroutine.create(function()
		Second = Bounds(Params("Hello", Face, 20))
	end)
	coroutine.resume(Again)
	print("the same face again:", coroutine.status(Again), Second)
	print("the same face from a metamethod:", FromMetamethod(Params("Hello", Face, 20)))
end
print("a face not loaded, from a metamethod:", FromMetamethod(Params("Hello", Font.fromEnum(Enum.Font.Creepster), 20)))
for _ = 1, 30 do
	task.wait()
end
print("the same face 30 frames later:", FromMetamethod(Params("Hello", Font.fromEnum(Enum.Font.Creepster), 20)))
print("SourceSansPro Medium from a metamethod:", FromMetamethod(Params("Hello", Font.new(Families .. "SourceSansPro.json", Enum.FontWeight.Medium), 20)))
print("SourceSansPro Thin from a metamethod:", FromMetamethod(Params("Hello", Font.new(Families .. "SourceSansPro.json", Enum.FontWeight.Thin), 20)))
print("SourceSansPro Heavy from a metamethod:", FromMetamethod(Params("Hello", Font.new(Families .. "SourceSansPro.json", Enum.FontWeight.Heavy), 20)))
do
	local Ready, Waits = {}, {}
	for _, Item in Enum.Font:GetEnumItems() do
		local Ok, Face = pcall(Font.fromEnum, Item)
		if Ok and Item.Name ~= "Unknown" then
			local Outcome = FromMetamethod(Params("Hi", Face, 14))
			table.insert(if string.sub(Outcome, 1, 3) == "ok " then Ready else Waits, Item.Name)
		end
	end
	table.sort(Ready)
	print("Enum.Font faces ready:", table.concat(Ready, " "))
end
print("FredokaOne:", Bounds(Params("Hello", Font.fromEnum(Enum.Font.FredokaOne), 20)))
print("FredokaOne Bold from a metamethod:", FromMetamethod(Params("Hello", Font.new(Families .. "FredokaOne.json", Enum.FontWeight.Bold), 20)))
print("FredokaOne Italic from a metamethod:", FromMetamethod(Params("Hello", Font.new(Families .. "FredokaOne.json", Enum.FontWeight.Regular, Enum.FontStyle.Italic), 20)))

-- GetTextBoundsParams: defaults, and what Size and Width store
do
	local Made = Instance.new("GetTextBoundsParams")
	print("defaults:", `"{Made.Text}"`, FaceText(Made.Font), P.num(Made.Size), P.num(Made.Width), Made.RichText)
	print("default bounds:", Bounds(Made))
	local Sized = Params("Hello", Font.fromEnum(Enum.Font.FredokaOne))
	for _, Size in { 14.5, 14.99, 15.5, 0.5, 0.2, 0, -3, 99.9, 100.4, 500, 0 / 0, 2 ^ 31, math.huge, -math.huge, "12", "x" } do
		local Ok, Message = pcall(function()
			Sized.Size = Size
		end)
		print("Size =", P.num(Size), "reads", if Ok then P.num(Sized.Size) else "error " .. P.err(Message), "measures", Bounds(Sized))
	end
	for _, Width in { -3, 0.5, 99.5, math.huge, 0 / 0 } do
		Made.Width = Width
		print("Width =", P.num(Width), "reads", P.num(Made.Width))
	end
	print("returns:", P.try(function()
		return select("#", TextService:GetTextBoundsAsync(Params("Hi")))
	end))
end

-- Every Enum.Font face against GetTextSize (asked after the bounds, which wait for the face): the same measurement but for
-- SourceSansItalic, which GetTextSize measures upright
local Pangram = "The quick brown fox jumps over the lazy dog"
local Ascii = {}
for Code = 33, 126 do
	table.insert(Ascii, string.char(Code))
end
local Samples = { Pangram, table.concat(Ascii) }
local function Signature(Face)
	local Parts = {}
	for _, Text in Samples do
		for _, Size in { 14, 20, 48 } do
			table.insert(Parts, Bounds(Params(Text, Face, Size)))
		end
	end
	return table.concat(Parts, " | ")
end
local EnumFaces = {}
for _, Item in Enum.Font:GetEnumItems() do
	local Ok, Face = pcall(Font.fromEnum, Item)
	if not Ok or Item.Name == "Unknown" then
		continue
	end
	local Measured = Signature(Face)
	local Parts = {}
	for _, Text in Samples do
		for _, Size in { 14, 20, 48 } do
			table.insert(Parts, Show(TextService:GetTextSize(Text, Size, Item, Vector2.new(1e6, 1e6))))
		end
	end
	table.insert(EnumFaces, { Item = Item, Face = Face, Measured = Measured })
	print("Enum.Font." .. Item.Name, if Measured == table.concat(Parts, " | ") then "measures as GetTextSize does" else "measures otherwise")
end

-- Faces no Enum.Font names: a family without the weight or style measures with one it has
local Faces = {
	{ "FredokaOne.json", "Bold", "Normal" },
	{ "FredokaOne.json", "Heavy", "Italic" },
	{ "SourceSansPro.json", "Medium", "Normal" },
	{ "SourceSansPro.json", "Thin", "Normal" },
	{ "SourceSansPro.json", "ExtraBold", "Normal" },
	{ "SourceSansPro.json", "Heavy", "Normal" },
	{ "SourceSansPro.json", "Bold", "Italic" },
	{ "GothamSSm.json", "Light", "Normal" },
	{ "GothamSSm.json", "SemiBold", "Normal" },
	{ "BuilderSans.json", "Heavy", "Normal" },
	{ "BuilderSans.json", "SemiBold", "Normal" },
	{ "Bangers.json", "Bold", "Italic" },
	{ "Arial.json", "Regular", "Italic" },
	{ "Roboto.json", "Bold", "Normal" },
}
for _, Entry in Faces do
	local Face = Font.new(Families .. Entry[1], Enum.FontWeight[Entry[2]], Enum.FontStyle[Entry[3]])
	local Measured = Signature(Face)
	local Same = "no Enum.Font face"
	for _, Known in EnumFaces do
		if Known.Face.Family == Face.Family and Known.Measured == Measured then
			Same = "Enum.Font." .. Known.Item.Name
			break
		end
	end
	print("face", FaceText(Face), "measures as", Same)
end
print("FredokaOne Bold:", Bounds(Params("Hello world", Font.new(Families .. "FredokaOne.json", Enum.FontWeight.Bold), 20)))

-- FredokaOne: sizes, widths that wrap, texts
local Fredoka = Font.fromEnum(Enum.Font.FredokaOne)
for _, Size in { 8, 10, 13, 15, 16, 24, 36, 48, 100 } do
	print("size", Size, Bounds(Params("Hello", Fredoka, Size)))
end
local Long = "The quick brown fox jumps over the lazy dog and keeps running"
for _, Width in { 0, 1, 10, 40, 100, 150, 220, 1000, -10, math.huge, 99.5 } do
	local Measured = Bounds(Params(Long, Fredoka, 14, Width))
	local Frame = if Width > 0 then Width else 1e6
	print("width", P.num(Width), Measured, if Measured == Show(TextService:GetTextSize(Long, 14, Enum.Font.FredokaOne, Vector2.new(Frame, 1e6))) then "as GetTextSize" else "not as GetTextSize")
end
local Texts = { "", "\n", "a\nb", "Hello\n", "\nHello", "a\n\nb", "  Hello  ", "Hello ", " ", "Hello world again and again" }
for _, Text in Texts do
	for _, Width in { 0, 30 } do
		print("text", `"{Text}"`, Width, Bounds(Params(Text, Fredoka, 20, Width)))
	end
end
for _, Text in { "a\tb", "h\195\169llo", "\230\151\165\230\156\172", "a\rb" } do
	for _, Width in { 0, 30 } do
		local Measured = Bounds(Params(Text, Fredoka, 20, Width))
		local Frame = if Width > 0 then Width else 1e6
		print("text", `"{Text}"`, Width, if Measured == Show(TextService:GetTextSize(Text, 20, Enum.Font.FredokaOne, Vector2.new(Frame, 1e6))) then "as GetTextSize" else "not as GetTextSize")
	end
end

-- RichText: markup is read (tags, entities, <br/>), and markup that does not parse raises
for _, Text in { "Hello", "<b>Hello</b>", "a &amp; b", "<font size=\"30\">Big</font> small", "x<br/>y", "<b>Hello", "a < b" } do
	print("rich", `"{Text}"`, Bounds(Params(Text, Fredoka, 20, 0, false)), "|", Bounds(Params(Text, Fredoka, 20, 0, true)))
end

-- Errors
print("nil:", P.try(function()
	return TextService:GetTextBoundsAsync(nil)
end))
print("no argument:", P.try(function()
	return TextService:GetTextBoundsAsync()
end))
print("a Part:", P.try(function()
	return TextService:GetTextBoundsAsync(Instance.new("Part"))
end))
print("a table:", P.try(function()
	return TextService:GetTextBoundsAsync({})
end))
print("a string:", P.try(function()
	return TextService:GetTextBoundsAsync("Hello")
end))
print("a number:", P.try(function()
	return TextService:GetTextBoundsAsync(5)
end))
print("a '.' call:", P.try(function()
	return TextService.GetTextBoundsAsync(Params("Hi"))
end))
print("a '.' call with the service:", P.try(function()
	return Show(TextService.GetTextBoundsAsync(TextService, Params("Hi")))
end))
print("an empty family:", Bounds(Params("Hi", Font.new(""))))
print("a family that is no file:", Bounds(Params("Hi", Font.new(Families .. "NoSuchFamily.json"))))
print("destroyed params:", P.try(function()
	local Made = Params("Hello", Fredoka, 20)
	Made:Destroy()
	return Show(TextService:GetTextBoundsAsync(Made))
end))
print("a Font assigned an Enum.Font:", P.try(function()
	Instance.new("GetTextBoundsParams").Font = Enum.Font.SourceSans
end))
