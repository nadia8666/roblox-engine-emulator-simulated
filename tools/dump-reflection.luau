-- Dumps the live engine API (ReflectionService) plus Instance.new defaults for every creatable class.
-- Writes data/reflection.json, which the engine compiles in.
-- Run it in Studio (edit mode, an empty place) with anything that saves the string the chunk returns as
-- data/reflection.json (a plugin, or a runner that executes Luau in Studio and keeps the result), then rebuild.
-- Nothing runs this by itself. Compare the new file with the old one first: `node tools/reflection-diff.mjs <old.json>
-- <new.json>` lists what came, went or changed (docs/engine.md "Data files").

local HttpService = game:GetService("HttpService")
local ReflectionService = game:GetService("ReflectionService")

local Encode

local function EncodeKeypoints(Sequence, Kind)
	local Result = {}
	for _, Keypoint in Sequence.Keypoints do
		if Kind == "ColorSequence" then
			table.insert(Result, { Keypoint.Time, Keypoint.Value.R, Keypoint.Value.G, Keypoint.Value.B })
		else
			table.insert(Result, { Keypoint.Time, Keypoint.Value, Keypoint.Envelope })
		end
	end
	return Result
end

function Encode(Value)
	local Kind = typeof(Value)
	if Kind == "nil" then
		return nil
	elseif Kind == "boolean" or Kind == "string" then
		return Value
	elseif Kind == "number" then
		if Value ~= Value then
			return { ["$t"] = "number", s = "nan" }
		elseif Value == math.huge then
			return { ["$t"] = "number", s = "inf" }
		elseif Value == -math.huge then
			return { ["$t"] = "number", s = "-inf" }
		end
		return Value
	elseif Kind == "EnumItem" then
		return { ["$t"] = "EnumItem", e = tostring(Value.EnumType), n = Value.Name }
	elseif Kind == "UDim" then
		return { ["$t"] = Kind, v = { Value.Scale, Value.Offset } }
	elseif Kind == "UDim2" then
		return { ["$t"] = Kind, v = { Value.X.Scale, Value.X.Offset, Value.Y.Scale, Value.Y.Offset } }
	elseif Kind == "Vector2" or Kind == "Vector2int16" then
		return { ["$t"] = Kind, v = { Value.X, Value.Y } }
	elseif Kind == "Vector3" or Kind == "Vector3int16" then
		return { ["$t"] = Kind, v = { Value.X, Value.Y, Value.Z } }
	elseif Kind == "Color3" then
		return { ["$t"] = Kind, v = { Value.R, Value.G, Value.B } }
	elseif Kind == "BrickColor" then
		return { ["$t"] = Kind, v = Value.Number }
	elseif Kind == "CFrame" then
		return { ["$t"] = Kind, v = { Value:GetComponents() } }
	elseif Kind == "Rect" then
		return { ["$t"] = Kind, v = { Value.Min.X, Value.Min.Y, Value.Max.X, Value.Max.Y } }
	elseif Kind == "NumberRange" then
		return { ["$t"] = Kind, v = { Value.Min, Value.Max } }
	elseif Kind == "ColorSequence" or Kind == "NumberSequence" then
		return { ["$t"] = Kind, v = EncodeKeypoints(Value, Kind) }
	elseif Kind == "Font" then
		return { ["$t"] = Kind, v = { Value.Family, Value.Weight.Name, Value.Style.Name } }
	elseif Kind == "Content" then
		return { ["$t"] = Kind, v = Value.Uri or "" }
	elseif Kind == "Instance" then
		return { ["$t"] = "Ref" }
	elseif Kind == "PhysicalProperties" then
		return { ["$t"] = Kind, s = tostring(Value) }
	end
	return { ["$t"] = Kind, s = tostring(Value) }
end

local function CopyTable(Value)
	if type(Value) ~= "table" then
		return Value
	end
	local Result = {}
	for Key, Item in Value do
		Result[tostring(Key)] = if type(Item) == "table" then CopyTable(Item) else tostring(Item)
	end
	return Result
end

local function TypeOf(TypeInfo)
	return {
		script = TypeInfo.ScriptType,
		engine = TypeInfo.EngineType,
		enum = TypeInfo.EnumType,
	}
end

local function Parameters(List)
	local Result = {}
	for _, Parameter in List or {} do
		table.insert(Result, { name = Parameter.Name, type = TypeOf(Parameter.Type) })
	end
	return Result
end

local Classes = {}
for _, Class in ReflectionService:GetClasses() do
	local Entry = {
		name = Class.Name,
		superclass = Class.Superclass,
		serialized = Class.Serialized,
		permits = CopyTable(Class.Permits),
		display = CopyTable(Class.Display),
		properties = {},
		events = {},
		methods = {},
		callbacks = {},
	}

	for _, Property in ReflectionService:GetPropertiesOfClass(Class.Name) do
		if Property.Owner == Class.Name then
			table.insert(Entry.properties, {
				name = Property.Name,
				type = TypeOf(Property.Type),
				serialized = Property.Serialized,
				permits = CopyTable(Property.Permits),
				display = CopyTable(Property.Display),
			})
		end
	end

	local EventsOk, Events = pcall(function()
		return (ReflectionService :: any):GetEventsOfClass(Class.Name)
	end)
	for _, Event in if EventsOk then Events else {} do
		if Event.Owner == Class.Name then
			table.insert(Entry.events, { name = Event.Name, parameters = Parameters(Event.Parameters) })
		end
	end

	local MethodsOk, Methods = pcall(function()
		return (ReflectionService :: any):GetMethodsOfClass(Class.Name)
	end)
	for _, Method in if MethodsOk then Methods else {} do
		if Method.Owner == Class.Name then
			table.insert(Entry.methods, {
				name = Method.Name,
				returns = TypeOf(Method.ReturnType or {}),
				parameters = Parameters(Method.Parameters),
				permits = CopyTable(Method.Permits),
			})
		end
	end

	local CallbacksOk, Callbacks = pcall(function()
		return (ReflectionService :: any):GetCallbacksOfClass(Class.Name)
	end)
	for _, Callback in if CallbacksOk then Callbacks else {} do
		if Callback.Owner == Class.Name then
			table.insert(Entry.callbacks, { name = Callback.Name, parameters = Parameters(Callback.Parameters) })
		end
	end

	local Created, Instance_ = pcall(Instance.new, Class.Name)
	if Created and Instance_ then
		local Defaults = {}
		for _, Property in ReflectionService:GetPropertiesOfClass(Class.Name) do
			local Read, Value = pcall(function()
				return (Instance_ :: any)[Property.Name]
			end)
			if Read then
				local EncodedOk, Encoded = pcall(Encode, Value)
				if EncodedOk and Encoded ~= nil then
					Defaults[Property.Name] = Encoded
				end
			end
		end
		Entry.defaults = Defaults
		pcall(Instance_.Destroy, Instance_)
	end

	table.insert(Classes, Entry)
end

local Enums = {}
for _, EnumType in Enum:GetEnums() do
	local Items = {}
	for _, Item in EnumType:GetEnumItems() do
		table.insert(Items, { name = Item.Name, value = Item.Value })
	end
	table.insert(Enums, { name = tostring(EnumType), items = Items })
end

return HttpService:JSONEncode({
	version = version(),
	classes = Classes,
	enums = Enums,
})
