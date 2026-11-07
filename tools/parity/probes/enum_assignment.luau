-- What an enum property takes and the text it refuses the rest with: nil, a boolean, a table, a datatype, an item of
-- another enum, a name it lacks, a number no item has, and the forms it reads (its name, its value as a number, a
-- numeric string, a number with a fraction), on a part and on a text label.
local Part = Instance.new("Part")
local Label = Instance.new("TextLabel")
local Values = {
	{ "nil", nil },
	{ "boolean", true },
	{ "table", {} },
	{ "vector3", Vector3.zero },
	{ "other-enum", Enum.SortOrder.Name },
	{ "lacking-name", "NotAName" },
	{ "lacking-number", 999999 },
	{ "name", "Wood" },
	{ "number", 512 },
	{ "numeric-string", "256" },
	{ "fraction", 512.5 },
}
for _, Case in Values do
	local PartResult = P.try(function()
		Part.Material = Case[2]
		return tostring(Part.Material)
	end)
	local LabelResult = P.try(function()
		Label.Font = Case[2]
		return tostring(Label.Font)
	end)
	print("enum " .. Case[1] .. " | " .. PartResult .. " | " .. LabelResult)
end
