--!nocheck
-- Vector3 as the native vector type (loaded by the prelude; lua/globals.rs).
--
-- In Roblox a Vector3 IS the native vector type (type() "vector", typeof() "Vector3"): the `vector`
-- library and Vector3 work on the same values, and libraries rely on it (Vide's spring keeps every
-- spring's state as vectors and returns a Vector3 spring's vector as its value; with a userdata
-- Vector3 its step raised on the first Vector3 spring, and Vide steps every spring of a VM in one
-- loop, so all of them stopped). Lune's Vector3 is a userdata, so scripts never see it: Vector3
-- values are native vectors (arithmetic, x / y / z, equality and tostring are the VM's), their
-- other members come from the vector type's metatable below. Lune's CFrame takes and returns
-- vectors itself (vendor patch: VectorArg, native_vector); its Ray / Region3 take and return them
-- through their libraries and metatables (ToLune / ToNative).
--
-- Measured in Studio 0.740 (the same script in both): Unit equals vector.normalize and Magnitude
-- vector.magnitude bit for bit, tostring prints each component as the VM prints a number
-- ("0.10000000149011612, 0.3333333432674408, 10000000000"), vectors are table keys by value,
-- getmetatable is locked.
--
-- (A file of its own so the prelude keeps its line numbers: script error traces name them.)

local R, Datatypes, NativeTypeof = ...

local LuneVector3New = Datatypes.Vector3.new
local CreateVector = vector.create

local function ToLune(Value)
	if type(Value) == "vector" then
		return LuneVector3New(Value.x, Value.y, Value.z)
	end
	return Value
end

local function ToNative(Value)
	if type(Value) == "userdata" and NativeTypeof(Value) == "Vector3" then
		return CreateVector(Value.X, Value.Y, Value.Z)
	end
	return Value
end

local function In(...)
	local Count = select("#", ...)
	if Count == 0 then
		return
	elseif Count == 1 then
		return ToLune((...))
	elseif Count == 2 then
		local A, B = ...
		return ToLune(A), ToLune(B)
	elseif Count == 3 then
		local A, B, C = ...
		return ToLune(A), ToLune(B), ToLune(C)
	end
	local Values = table.pack(...)
	for Index = 1, Count do
		Values[Index] = ToLune(Values[Index])
	end
	return table.unpack(Values, 1, Count)
end

local function Out(...)
	local Count = select("#", ...)
	if Count == 0 then
		return
	elseif Count == 1 then
		return ToNative((...))
	elseif Count == 2 then
		local A, B = ...
		return ToNative(A), ToNative(B)
	elseif Count == 3 then
		local A, B, C = ...
		return ToNative(A), ToNative(B), ToNative(C)
	end
	local Values = table.pack(...)
	for Index = 1, Count do
		Values[Index] = ToNative(Values[Index])
	end
	return table.unpack(Values, 1, Count)
end

-- A member read from a Lune datatype: its methods take and return vectors
local ConvertedMethods = setmetatable({}, { __mode = "k" })

local function Converted(Value)
	if type(Value) == "function" then
		local Method = ConvertedMethods[Value]
		if Method == nil then
			Method = function(...)
				return Out(Value(In(...)))
			end
			ConvertedMethods[Value] = Method
		end
		return Method
	end
	return ToNative(Value)
end

-- Members the VM does not provide itself, over the `vector` library
local Magnitude = vector.magnitude
local Normalize = vector.normalize
local Dot = vector.dot
local Cross = vector.cross

local Charge, LerpCost, FuzzyEqCost, Atan2f = R.Charge, R.VectorLerpCost, R.VectorFuzzyEqCost, R.Atan2f

local function Lerp(Self, Goal, Alpha)
	Charge(LerpCost)
	return Self + (Goal - Self) * Alpha
end

local VectorMethods = {
	Dot = Dot,
	Cross = Cross,
	Abs = vector.abs,
	Ceil = vector.ceil,
	Floor = vector.floor,
	Sign = vector.sign,
	Max = vector.max,
	Min = vector.min,
	Lerp = Lerp,
	lerp = Lerp,
	FuzzyEq = function(Self, Other, Epsilon)
		Charge(FuzzyEqCost)
		local Difference = vector.abs(Self - Other)
		local Limit = Epsilon or 1e-5
		return Difference.x <= Limit and Difference.y <= Limit and Difference.z <= Limit
	end,
	Angle = function(Self, Other, Axis)
		local Normal = Cross(Self, Other)
		local Angle = Atan2f(Magnitude(Normal), Dot(Self, Other)) -- atan2f in floats, as a playing client (cframe_order probe)
		if Axis ~= nil and Dot(Normal, Axis) < 0 then
			return -Angle
		end
		return Angle
	end,
}

-- The VM answers a vector's own members: x / y / z in either case (a key the compiler could not fold, `v[axis]`,
-- included) and Magnitude / Unit in either case (luaV_vectormember in patches/luau/05-vector-members.patch: the work clock charges them
-- Studio's 10-12 ns, where a function here cost a 15 ns host call). Methods are a table lookup, as in Studio
-- (a:Dot(b) 9-10 ns); the table's own metatable raises Studio's error for any other member, from the script's line.
R.SetVectorMetatable({
	__index = setmetatable(VectorMethods, {
		__index = function(_, Key)
			error(`{Key} is not a valid member of Vector3`, 0)
		end,
	}),
	__metatable = "The metatable is locked",
})

-- Lune's Ray / Region3 values: members and operators through ToLune / ToNative; Aliases
-- maps the deprecated names Roblox still answers to their current ones
local function ConvertMetatable(Sample, Aliases)
	local Meta = R.RawMetatable(Sample)
	local Index = rawget(Meta, "__index")
	if type(Index) == "function" then
		rawset(Meta, "__index", function(Self, Key)
			return Converted(Index(Self, Aliases and Aliases[Key] or Key))
		end)
	elseif type(Index) == "table" then
		rawset(Meta, "__index", function(_, Key)
			return Converted(Index[Aliases and Aliases[Key] or Key])
		end)
	end
	for _, Key in { "__add", "__sub", "__mul", "__div" } do
		local Operator = rawget(Meta, Key)
		if type(Operator) == "function" then
			rawset(Meta, Key, function(A, B)
				return ToNative(Operator(ToLune(A), ToLune(B)))
			end)
		end
	end
end

local Zero = LuneVector3New(0, 0, 0)
ConvertMetatable(Datatypes.Ray.new(Zero, Zero))
ConvertMetatable(Datatypes.Region3.new(Zero, Zero))

-- The script library's own members (the prelude's ScriptLibrary takes these before Lune's)
local Library = {
	zero = vector.zero,
	one = vector.one,
	xAxis = CreateVector(1, 0, 0),
	yAxis = CreateVector(0, 1, 0),
	zAxis = CreateVector(0, 0, 1),
	min = vector.min,
	max = vector.max,
}

-- Lune libraries whose functions take or return Vector3 values (the Vector3 library is native)
local VectorLibraries = { Ray = true, Region3 = true }

-- Ray as a plain frozen table of native vectors: Origin and Direction are raw fields (a read is a table slot, not
-- a userdata member with a Lune Vector3 each way), typeof says "Ray" through __type. Members and the closest
-- point follow Lune's (a point clamped to the ray's length).
local RayMeta
local function NewRay(Origin, Direction)
	if type(Origin) ~= "vector" then
		error(`invalid argument #1 to 'new' (Vector3 expected, got {typeof(Origin)})`, 2)
	end
	if type(Direction) ~= "vector" then
		error(`invalid argument #2 to 'new' (Vector3 expected, got {typeof(Direction)})`, 2)
	end
	return table.freeze(setmetatable({ Origin = Origin, Direction = Direction }, RayMeta))
end
local function RayClosestPoint(Self, Point)
	local Unit = Self.Direction.Unit
	return Self.Origin + Unit * math.clamp(Dot(Point - Self.Origin, Unit), 0, Magnitude(Self.Direction))
end
local function RayDistance(Self, Point)
	return Magnitude(RayClosestPoint(Self, Point) - Point)
end
RayMeta = {
	__type = "Ray",
	__metatable = "The metatable is locked",
	__index = function(Self, Key)
		if Key == "Unit" then
			return NewRay(Self.Origin, Self.Direction.Unit)
		elseif Key == "ClosestPoint" then
			return RayClosestPoint
		elseif Key == "Distance" then
			return RayDistance
		end
		error(`{Key} is not a valid member of Ray`, 0)
	end,
	__tostring = function(Self)
		return "{" .. tostring(Self.Origin) .. "}, {" .. tostring(Self.Direction) .. "}"
	end,
}
local RayLibrary = { new = NewRay }

return ToLune, ToNative, In, Out, VectorLibraries, Library, RayLibrary
