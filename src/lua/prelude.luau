--!nocheck
-- Rust engine prelude (native/src/engine/lua): the Luau side of the engine's script API.
--
-- 1. The call protocol: Rust functions return values normally, or SIGNAL followed by
--    (1, message, level) to raise a string error (level 1: the position of the script line that
--    called the engine function, 0: none) or (2) to yield the calling coroutine (the Rust side has
--    already registered its resumption). `wrap` turns such a function into a script function.
-- 2. typeof with registered table types (SharedTable, Mouse).
-- 3. Script libraries over Lune's datatypes (port of runtime/engine/datatypes.luau ScriptLibrary,
--    constructor overrides and extras) and the small proxy types datatypes.luau implements in Luau
--    (Faces, Axes, Path2DControlPoint, parameter objects).
-- 4. Pieces of the shared global environment that are plain Luau (runtime/engine/globals.luau).

local R, Roblox = ...
local SIGNAL = R.SIGNAL

local error = error
local select = select
local yield = coroutine.yield
local NativeTypeof = typeof

-- (exactly the values the engine function returned: one that returns nothing returns no values, as Roblox's do,
-- `select("#", Instance.ClearAllChildren(Part))` is 0)
local function Pass(...)
	local First = ...
	if First == SIGNAL then
		local _, Kind, Message, Level = ...
		if Kind == 1 then
			if Level == 0 then
				error(Message, 0)
			end
			-- 1 = Pass, 2 = the wrapper, 3 = its caller
			error(Message, Level + 2)
		end
		return yield()
	end
	return ...
end

local function Wrap(Function)
	return function(...)
		return Pass(Function(...))
	end
end

-- Instance __index / __newindex: fixed-arity Rust functions returning (value, message, level)
local function WrapIndex(Function)
	return function(Self, Key)
		local Value, Message, Level = Function(Self, Key)
		if Message then
			error(Message, if Level == 0 then 0 else Level + 1)
		end
		return Value
	end
end

local function WrapNewIndex(Function)
	return function(Self, Key, Value)
		local Message, Level = Function(Self, Key, Value)
		if Message then
			error(Message, if Level == 0 then 0 else Level + 1)
		end
	end
end

-- typeof

local Registry = setmetatable({}, { __mode = "k" })

local function Typeof(Value)
	local Native = NativeTypeof(Value)
	-- Only tables and proxies are registered (engine userdata carry their __type)
	if Native == "table" or Native == "userdata" then
		return Registry[Value] or Native
	end
	-- In Roblox the native vector type is Vector3
	if Native == "vector" then
		return "Vector3"
	end
	return Native
end

local function Register(Value, TypeName)
	Registry[Value] = TypeName
	return Value
end

-- Userdata helper: a proxy with a locked metatable whose members come from `Index`.

local function ReadOnlyMember(_, Key)
	error(`{Key} cannot be assigned to`, 0)
end

local function Proxy(TypeName, Index, NewIndex, Extra)
	local Object = newproxy(true)
	local Meta = getmetatable(Object)
	Meta.__metatable = "The metatable is locked"
	Meta.__index = Index
	Meta.__newindex = NewIndex or ReadOnlyMember
	if Extra then
		for Key, Value in Extra do
			Meta[Key] = Value
		end
	end
	Meta.__type = TypeName
	return Object
end

-- Datatypes (datatypes.luau)

local Datatypes = {}
for _, Name in {
	"Axes", "BrickColor", "CFrame", "Color3", "ColorSequence", "ColorSequenceKeypoint", "Faces",
	"NumberRange", "NumberSequence", "NumberSequenceKeypoint", "PhysicalProperties", "Ray", "Rect",
	"Region3", "Region3int16", "UDim", "UDim2", "Vector2", "Vector2int16", "Vector3", "Vector3int16",
} do
	Datatypes[Name] = Roblox[Name]
end

local Color3 = Datatypes.Color3
local Vector2 = Datatypes.Vector2
local ToLune, ToNative, In, Out, VectorLibraries, VectorLibrary, RayLibrary = R.Vector3(R, Datatypes, NativeTypeof) -- (lua/vector3.luau)
local UDim = Datatypes.UDim
local UDim2 = Datatypes.UDim2

-- Lune's two-value sequence constructors reuse the first value for the end keypoint
do
	local LuneColorSequence = Datatypes.ColorSequence
	local LuneNumberSequence = Datatypes.NumberSequence
	Datatypes.ColorSequence = setmetatable({
		new = function(First, Second)
			if NativeTypeof(First) == "Color3" then
				local Last = if NativeTypeof(Second) == "Color3" then Second else First
				return LuneColorSequence.new({
					Datatypes.ColorSequenceKeypoint.new(0, First),
					Datatypes.ColorSequenceKeypoint.new(1, Last),
				})
			end
			return LuneColorSequence.new(First)
		end,
	}, { __index = LuneColorSequence })
	Datatypes.NumberSequence = setmetatable({
		new = function(First, Second)
			if type(First) == "number" then
				local Last = if type(Second) == "number" then Second else First
				return LuneNumberSequence.new({
					Datatypes.NumberSequenceKeypoint.new(0, First),
					Datatypes.NumberSequenceKeypoint.new(1, Last),
				})
			end
			return LuneNumberSequence.new(First)
		end,
	}, { __index = LuneNumberSequence })
end

local function RobloxErrorMessage(Error, FunctionName)
	local Text = tostring(Error)
	Text = string.match(Text, "^(.-)\nstack traceback:") or Text
	Text = string.gsub(Text, "^runtime error: ", "")
	local Index, From, To = string.match(Text, "^bad argument #(%d+)[^:]*: error converting Lua (%w+) to ([%w_]+)")
	if Index then
		local Expected = To
		if string.match(To, "^[fiu]%d+$") then
			Expected = "number"
		elseif To == "userdata" then
			Expected = string.match(Text, "of type '([%w]+)'") or "userdata"
		end
		return `invalid argument #{Index} to '{FunctionName}' ({Expected} expected, got {From})`
	end
	return Text
end

local function PassResults(FunctionName, Ok, ...)
	if Ok then
		return ...
	end
	local Failure = ...
	if type(Failure) == "string" then
		error(Failure, 0)
	end
	error(RobloxErrorMessage(Failure, FunctionName), 0)
end

local function Float(Value)
	if type(Value) == "number" then
		return Value
	end
	return tonumber(Value) or 0
end

local function Int32(Value)
	local Number = Float(Value)
	if Number ~= Number or Number >= 2147483648 or Number < -2147483648 then
		return -2147483648
	end
	return if Number >= 0 then math.floor(Number) else math.ceil(Number)
end

local function VectorParts(Value)
	if NativeTypeof(Value) == "vector" then
		return Value.x, Value.y, Value.z
	end
	return Value.X, Value.Y, Value.Z
end

-- CFrame has no override here: Lune's CFrame takes and returns native vectors itself (vendor patch:
-- VectorArg, native_vector; CFrame.new(from, to) looks at the target), so a script's CFrame math
-- runs as native code, as in Roblox, and the library wrapper below only translates its errors.
-- (These comment lines stand where the CFrame.new override was: the prelude's line numbers, which
-- script error traces name, stay as they were.)
--
--
-- The other overrides pass a default or clamp what Lune's constructors would reject or wrap
-- (Float, Int32; the Roblox rules are in the constructors test of lua/datatype_tests.rs):
local ConstructorOverrides = {
	UDim = {
		new = function(Scale, Offset)
			return UDim.new(Float(Scale), Int32(Offset))
		end,
	},
	UDim2 = {
		new = function(...)
			local First = ...
			if NativeTypeof(First) == "UDim" then
				local Second = select(2, ...)
				return PassResults("new", pcall(UDim2.new, First, Second))
			end
			local XScale, XOffset, YScale, YOffset = ...
			return UDim2.new(Float(XScale), Int32(XOffset), Float(YScale), Int32(YOffset))
		end,
		fromOffset = function(X, Y)
			return UDim2.fromOffset(Int32(X), Int32(Y))
		end,
		fromScale = function(X, Y)
			return UDim2.fromScale(Float(X), Float(Y))
		end,
	},
	Vector2 = {
		new = function(X, Y)
			return Vector2.new(Float(X), Float(Y))
		end,
	},
	Vector3 = {
		new = function(X, Y, Z)
			return vector.create(Float(X), Float(Y), Float(Z))
		end,
	},
	Color3 = {
		new = function(Red, Green, Blue)
			return Color3.new(Float(Red), Float(Green), Float(Blue))
		end,
		fromRGB = function(Red, Green, Blue)
			return Color3.new(Float(Red) / 255, Float(Green) / 255, Float(Blue) / 255)
		end,
	},
}

local EnumItemName = R.EnumItemName
local EnumItemType = R.EnumItemType
local GetEnumItem = R.GetEnumItem

local LibraryExtras = {
	Color3 = {
		toHSV = function(Color)
			return Color:ToHSV()
		end,
	},
	Vector3 = {
		FromNormalId = function(Item)
			local Name = EnumItemName(Item)
			local Directions = {
				Right = vector.create(1, 0, 0),
				Left = vector.create(-1, 0, 0),
				Top = vector.create(0, 1, 0),
				Bottom = vector.create(0, -1, 0),
				Back = vector.create(0, 0, 1),
				Front = vector.create(0, 0, -1),
			}
			return Directions[Name or ""] or vector.zero
		end,
		FromAxis = function(Item)
			local Name = EnumItemName(Item)
			return if Name == "X" then vector.create(1, 0, 0) elseif Name == "Y" then vector.create(0, 1, 0) elseif Name == "Z" then vector.create(0, 0, 1) else vector.zero
		end,
	},
	CFrame = {
		lookAlong = function(At, Direction, Up)
			return Datatypes.CFrame.lookAlong(At, Direction, Up)
		end,
	},
	BrickColor = {
		Random = function()
			return Datatypes.BrickColor.random()
		end,
	},
}
LibraryExtras.Vector3.fromNormalId = LibraryExtras.Vector3.FromNormalId
LibraryExtras.Vector3.fromAxis = LibraryExtras.Vector3.FromAxis; for Key, Value in VectorLibrary do ConstructorOverrides.Vector3[Key] = Value end; ConstructorOverrides.Ray = RayLibrary -- (native zero, one, axes, min, max)

local ScriptLibraries = {}

local function ScriptLibrary(Name)
	local Existing = ScriptLibraries[Name]
	if Existing then
		return Existing
	end
	local Library = Datatypes[Name]
	local Extras = LibraryExtras[Name] or {}
	local Overrides = ConstructorOverrides[Name] or {}
	local Wrapped, Converts = {}, VectorLibraries[Name]
	local function Resolve(_, Key)
		local Value = Overrides[Key]
		if Value ~= nil then
			return Value
		end
		Value = Extras[Key]
		if Value == nil then
			Value = Library[Key]
		end
		if type(Value) ~= "function" then
			return Value
		end
		local Cached = Wrapped[Key]
		if Cached == nil then
			Cached = if Converts then function(...) return Out(PassResults(Key, pcall(Value, In(...)))) end else function(...)
				return PassResults(Key, pcall(Value, ...))
			end
			Wrapped[Key] = Cached
		end
		return Cached
	end
	-- Members resolve once, then come from a plain __index table (the VM's fast path)
	local Members = {}
	setmetatable(Members, {
		__index = function(_, Key)
			local Value = Resolve(nil, Key)
			if Value ~= nil then
				rawset(Members, Key, Value)
			end
			return Value
		end,
	})
	local Result = setmetatable({}, {
		__index = Members,
		__newindex = function(_, Key)
			error(`{Key} cannot be assigned to`, 0)
		end,
		__metatable = "The metatable is locked",
	})
	ScriptLibraries[Name] = Result
	return Result
end

-- Faces / Axes (Lune's versions only accept Lune's own EnumItems)

local function FlagSet(TypeName, Names, Resolve, Shown)
	local Library = {}
	local Info = setmetatable({}, { __mode = "k" })
	local Meta = {
		__eq = function(A, B)
			local X, Y = Info[A], Info[B]
			if X == nil or Y == nil then
				return false
			end
			for _, Name in Names do
				if X[Name] ~= Y[Name] then
					return false
				end
			end
			return true
		end,
		__tostring = function(Self) -- (the names in Shown's order: Roblox prints Faces by NormalId value, Axes as X, Y, Z only)
			local Parts = {}
			for _, Name in Shown or Names do
				if Info[Self][Name] then
					table.insert(Parts, Name)
				end
			end
			return table.concat(Parts, ", ")
		end,
	}
	function Library.new(...)
		local Flags = {}
		for _, Name in Names do
			Flags[Name] = false
		end
		for Index = 1, select("#", ...) do
			local Item = select(Index, ...)
			for _, Name in Resolve(Item) do
				Flags[Name] = true
			end
		end
		local Object = Proxy(TypeName, function(Self, Key)
			local Value = Info[Self][Key]
			if Value == nil then
				error(`{Key} is not a valid member of {TypeName}`, 0)
			end
			return Value
		end, nil, Meta)
		Info[Object] = Flags
		return Object
	end
	return Library
end

local FaceNames = { "Top", "Bottom", "Left", "Right", "Back", "Front" }
local AxisFaces = { X = { "Left", "Right" }, Y = { "Top", "Bottom" }, Z = { "Back", "Front" } }

local Faces = FlagSet("Faces", FaceNames, function(Item)
	local Name = EnumItemName(Item)
	return if Name and EnumItemType(Item) == "NormalId" then { Name } else {}
end, { "Right", "Top", "Back", "Left", "Bottom", "Front" })

local Axes = FlagSet("Axes", { "X", "Y", "Z", "Top", "Bottom", "Left", "Right", "Back", "Front" }, function(Item)
	local Name = EnumItemName(Item)
	local TypeName = EnumItemType(Item)
	if TypeName == "Axis" and Name then
		return { Name, table.unpack(AxisFaces[Name] or {}) }
	elseif TypeName == "NormalId" and Name then
		for Axis, List in AxisFaces do
			if table.find(List, Name) then
				return { Name, Axis }
			end
		end
	end
	return {}
end, { "X", "Y", "Z" })

-- datatypes.luau replaces Lune's Faces / Axes with these before the script libraries wrap them
Datatypes.Faces = Faces
Datatypes.Axes = Axes

local Path2DControlPoint = {}
-- (Path2DControlPoint.new and its values are defined below the traced functions, beside the other
-- additions; these lines keep the count of the ones that built it here: rur's golden traces record
-- prelude line numbers.)
--
--
--
--
--
--
--
--
--
--
--

local function ParamsType(TypeName, Defaults, Methods, Meta)
	return {
		new = function(Initial)
			local Data = table.clone(Defaults)
			for Key, Value in Initial or {} do
				Data[Key] = Value
			end
			return Proxy(TypeName, function(_, Key)
				return if Methods and Methods[Key] then function(_Self, ...) return Methods[Key](Data, ...) end else Data[Key]
			end, function(_, Key, Value)
				Data[Key] = Value
			end, Meta and Meta(Data))
		end,
	}
end

local function InertLibrary(TypeName)
	return {
		new = function(...)
			local Fields = table.pack(...)
			return Proxy(TypeName, function(_, Key)
				return Fields[Key]
			end)
		end,
	}
end

-- Shared environment pieces (globals.luau)

local SharedTable = {}
function SharedTable.new(Initial)
	local Self = if Initial then table.clone(Initial) else {}
	Register(Self, "SharedTable")
	return Self
end
function SharedTable.clone(Value, _Deep)
	return SharedTable.new(Value)
end
SharedTable.cloneAndFreeze = SharedTable.clone
function SharedTable.size(Value)
	local Count = 0
	for _ in Value do
		Count += 1
	end
	return Count
end
function SharedTable.clear(Value)
	table.clear(Value)
end
function SharedTable.isFrozen(_Value)
	return false
end
function SharedTable.update(Value, Key, Callback)
	Value[Key] = Callback(Value[Key])
end
function SharedTable.increment(Value, Key, Delta)
	local Old = Value[Key] or error(`Unable to increment the value for key "{Key}" because it does not exist in the SharedTable.`, 0)
	Value[Key] = Old + Delta; R.Charge(R.SharedIncrementCost)
	return Old
end

local function MakeStubTree(Name)
	local Cache = {}
	return setmetatable({}, {
		__index = function(_, Key)
			if Key == "GetFFlag" or Key == "GetFVariable" or Key == "GetFInt" then
				return function()
					return nil
				end
			end
			local Existing = Cache[Key]
			if Existing == nil then
				Existing = MakeStubTree(`{Name}.{Key}`)
				Cache[Key] = Existing
			end
			return Existing
		end,
		__newindex = function(_, Key, Value)
			Cache[Key] = Value
		end,
		__tostring = function()
			return Name
		end,
		__call = function()
			return MakeStubTree(`{Name}()`)
		end,
	})
end

-- Roblox's utf8 additions: graphemes and the normalization functions (identity here)
local Utf8Table = table.clone(utf8)
local function Extends(Code)
	return (Code >= 0x0300 and Code <= 0x036F)
		or (Code >= 0x1AB0 and Code <= 0x1AFF)
		or (Code >= 0x1DC0 and Code <= 0x1DFF)
		or (Code >= 0x20D0 and Code <= 0x20FF)
		or (Code >= 0xFE00 and Code <= 0xFE0F)
		or (Code >= 0xFE20 and Code <= 0xFE2F)
		or (Code >= 0x1F3FB and Code <= 0x1F3FF)
		or (Code >= 0xE0020 and Code <= 0xE007F)
		or (Code >= 0xE0100 and Code <= 0xE01EF)
		or Code == 0x200D
end
Utf8Table.graphemes = function(Text, First, Last)
	local Clusters = {}
	local Previous = nil
	local Joining = false
	local Flags = 0
	for Position, Code in utf8.codes(string.sub(Text, 1, Last or #Text)) do
		if Position < (First or 1) then
			continue
		end
		local Continue = Previous ~= nil and (Extends(Code) or Joining or (Previous == 13 and Code == 10))
		if Code >= 0x1F1E6 and Code <= 0x1F1FF then
			Continue = Continue or (Flags % 2 == 1)
			Flags += 1
		else
			Flags = 0
		end
		if Continue and #Clusters > 0 then
			Clusters[#Clusters][2] = Position + #utf8.char(Code) - 1
		else
			table.insert(Clusters, { Position, Position + #utf8.char(Code) - 1 })
		end
		Joining = Code == 0x200D
		Previous = Code
	end
	local Index = 0
	return function()
		Index += 1
		local Cluster = Clusters[Index]
		if Cluster then
			return Cluster[1], Cluster[2]
		end
		return nil
	end
end
Utf8Table.nfcnormalize = function(Text)
	return Text
end
Utf8Table.nfdnormalize = function(Text)
	return Text
end

local DebugTable = table.clone(debug)
DebugTable.profilebegin = function() end
DebugTable.profileend = function() end
DebugTable.setmemorycategory = function() end
DebugTable.resetmemorycategory = function() end
DebugTable.dumpcodesize = function() end
DebugTable.getmemorycategory = function()
	return "default"
end

-- task.wait / wait / Signal:Wait / WaitForChild yield in Luau after the Rust side registered the
-- thread (or refused to: Pass raises it, lua/refs.rs); everything else is a Rust function wrapped for string errors.
local ScheduleWait = R.ScheduleWait
local ScheduleLegacyWait = R.ScheduleLegacyWait

local Task = {
	spawn = Wrap(R.TaskSpawn),
	defer = Wrap(R.TaskDefer),
	delay = Wrap(R.TaskDelay),
	wait = function(Duration)
		Pass(ScheduleWait(Duration))
		return yield()
	end,
	-- (the running thread, and one waiting on a coroutine it resumed, cannot be cancelled: Roblox raises this without a
	-- position; measured in a playing client, Studio 0.744, tools/parity/probes/task_order.luau)
	cancel = function(...)
		local Thread = ...
		if type(Thread) == "thread" then
			local Status = coroutine.status(Thread)
			if Status == "running" or Status == "normal" then
				error("cannot cancel thread", 0)
			end
		end
		return Pass(R.TaskCancel(...))
	end,
	synchronize = function() if R.ParallelPark(true) then yield() end end, -- (a parallel handler's thread leaves the phase: actors.rs)
	desynchronize = function() if R.ParallelPark(false) then yield() end end,
}

local function LegacyWait(Duration)
	Pass(ScheduleLegacyWait(Duration))
	return yield()
end

local function Traceback(Thread)
	return debug.traceback(Thread)
end

-- print / warn (output.luau Stringify)
local Output = R.Output
local function Stringify(...)
	local Count = select("#", ...)
	local Parts = table.create(Count)
	for Index = 1, Count do
		Parts[Index] = tostring((select(Index, ...)))
	end
	return table.concat(Parts, " ")
end
local function Print(...)
	Output("print", Stringify(...))
end
local function Warn(...)
	Output("warn", Stringify(...))
end

-- require (modules.luau Require): the Rust side keeps the module records; the chunk runs here so
-- a module may yield while it loads. RequireBegin answers
--   0, value            the module's (cached) result
--   1, message, level   an error (level 1: the requiring script's line, 0: none)
--   2                   loading on another thread: wait (resumed when it finishes), then retry
--   3, chunk, key       run the chunk, then report through RequireFinish
local RequireBegin = R.RequireBegin
local RequireFinish = R.RequireFinish
local pcall = pcall
local function Require(Target, Caller, Vm)
	while true do
		local Status, A, B = RequireBegin(Target, Caller, Vm)
		if Status == 0 then
			return A
		elseif Status == 1 then
			if B == 0 then
				error(A, 0)
			end
			-- 1 = Require, 2 = the script's require function, 3 = the requiring line
			error(A, 3)
		elseif Status == 2 then
			yield()
		else
			local Finished, Message, Level = RequireFinish(B, table.pack(pcall(A)), Vm)
			if Finished then
				return Message
			end
			if Level == 0 then
				error(Message, 0)
			end
			error(Message, 3)
		end
	end
end

-- Engine.Call's thread body: the callback's results or failure (message + traceback) land in Box
local function CallBody(Box, Callback, ...)
	local Packed = table.pack(xpcall(Callback, function(Message)
		return { Message = Message, Trace = debug.traceback(nil, 2) }
	end, ...))
	if Packed[1] then
		Box.Results = Packed
	else
		Box.Failure = Packed[2]
		Box.Message = tostring(Packed[2].Message)
	end
end

local VmGlobals = {} -- an Actor's _G, by its key (modules.rs vm_key): its VM's own, as in Studio
local function Environment(Shared, Script, Vm)
	local Env = {
		script = Script,
		require = function(Target)
			return Require(Target, Script, Vm)
		end,
	}
	if Vm ~= nil then
		local Globals = VmGlobals[Vm]
		if Globals == nil then
			Globals = {}
			VmGlobals[Vm] = Globals
		end
		Env._G = Globals
	end
	return setmetatable(Env, { __index = Shared })
end

-- RaycastParams / OverlapParams:AddToFilter(Instance | {Instance}) adds to FilterDescendantsInstances
-- (documented), as a new list: a table the script assigned earlier is not changed under it. (Kept below
-- the traced functions: rur's golden traces record prelude line numbers.)
local FilterMethods = {
	AddToFilter = function(Data, Instances)
		local List = table.clone(Data.FilterDescendantsInstances or {})
		if Typeof(Instances) == "Instance" then
			table.insert(List, Instances)
		elseif type(Instances) == "table" then
			for _, Item in Instances do
				if Typeof(Item) ~= "Instance" then
					error(`Unable to cast {Typeof(Item)} to Instance`, 3)
				end
				table.insert(List, Item)
			end
		else
			error(`Unable to cast {Typeof(Instances)} to Instance`, 3)
		end
		Data.FilterDescendantsInstances = List
	end,
}

-- RaycastParams / OverlapParams. Their instance lists, FilterDescendantsInstances and Roblox's spatial query filters of April
-- 2026, ExcludeInstances and IncludeInstances ({Instance}?, nil until assigned), hold a copy of the table assigned and answer
-- a copy, as Roblox's array properties do: changing a table afterwards changes no filter (a script assigns it again, as
-- Roblox's engineer advises in the announcement's thread). The queries read what is stored through ParamsData
-- (world/query.rs), not through the proxy.
local ParamsData = setmetatable({}, { __mode = "k" })
local FilterLists = { FilterDescendantsInstances = true, ExcludeInstances = true, IncludeInstances = true }
local function ListCopy(Value)
	return if type(Value) == "table" then table.move(Value, 1, #Value, 1, table.create(#Value)) else Value
end
local function FilterParamsType(TypeName, Defaults, Meta)
	return {
		new = function(Initial)
			local Data = table.clone(Defaults)
			for Key, Value in Initial or {} do
				Data[Key] = if FilterLists[Key] then ListCopy(Value) else Value
			end
			local Object = Proxy(TypeName, function(_, Key)
				local Method = FilterMethods[Key]
				if Method then
					return function(_Self, ...)
						return Method(Data, ...)
					end
				end
				local Value = Data[Key]
				return if FilterLists[Key] then ListCopy(Value) else Value
			end, function(_, Key, Value)
				Data[Key] = if FilterLists[Key] then ListCopy(Value) else Value
			end, Meta and Meta(Data))
			ParamsData[Object] = Data
			return Object
		end,
	}
end

-- coroutine.create / wrap make a thread, which Studio charges the script for as task.spawn does (cost.rs THREAD)
local Coroutine = table.clone(coroutine)
do
	local Charge, Thread = R.Charge, R.ThreadCost
	local Create, NativeWrap = coroutine.create, coroutine.wrap
	Coroutine.create = function(Function)
		Charge(Thread)
		return Create(Function)
	end
	Coroutine.wrap = function(Function)
		Charge(Thread)
		return NativeWrap(Function)
	end
end

-- tostring of the parameter objects as a playing client prints them (Studio 0.744): Roblox's fields in its order, an
-- instance list as {Name} for one instance and {N instances...} for more, an empty enum list as nothing, numbers with 9
-- significant digits. (A list of enum items with members is printed by name, comma-separated: not measured.)
local function ParamsText(TypeName, Fields)
	return function(Data)
		return {
			__tostring = function()
				local Parts = {}
				for _, Field in Fields do
					local Name, Value = Field[1], Data[Field[1]]
					if Value == nil then
						Value = Field[2]
					end
					local Text
					if type(Value) == "table" and Name == "FilterDescendantsInstances" then
						Text = if #Value == 0 then "{}" elseif #Value == 1 then `\{{Value[1]}\}` else `\{{#Value} instances...\}`
					elseif type(Value) == "table" then
						local Names = {}
						for _, Item in Value do
							table.insert(Names, EnumItemName(Item) or tostring(Item))
						end
						Text = table.concat(Names, ", ")
					elseif type(Value) == "number" then
						Text = string.format("%.9g", Value)
					else
						Text = EnumItemName(Value) or tostring(Value)
					end
					table.insert(Parts, `{Name}={Text}`)
				end
				return `{TypeName}\{{table.concat(Parts, ", ")}\}`
			end,
		}
	end
end
local RaycastParamsText = ParamsText("RaycastParams", { { "IgnoreWater", false }, { "Margin", 0 }, { "SizeGradient", 0 }, { "BruteForceAllSlow", false },
	{ "RespectCanCollide", false }, { "CollisionGroup", "Default" }, { "FilterDescendantsInstances", {} } })
local OverlapParamsText = ParamsText("OverlapParams", { { "MaxParts", 0 }, { "Tolerance", 0 }, { "BruteForceAllSlow", false }, { "RespectCanCollide", false },
	{ "CollisionGroup", "Default" }, { "FilterDescendantsInstances", {} } })
-- CatalogSearchParams.new(): Roblox's defaults as its tostring shows them (made at the first call, once the enums exist)
local CatalogSearchParamsType
local CatalogSearchParamsLibrary = {
	new = function(Initial)
		if not CatalogSearchParamsType then
			local Fields = { { "SearchKeyword", "" }, { "CategoryFilter", GetEnumItem("CatalogCategoryFilter", "None") }, { "SortType", GetEnumItem("CatalogSortType", "Relevance") },
				{ "SortAggregation", GetEnumItem("CatalogSortAggregation", "AllTime") }, { "SalesTypeFilter", GetEnumItem("SalesTypeFilter", "All") }, { "CreatorName", "" },
				{ "Limit", 30 }, { "CreatorType", GetEnumItem("CreatorTypeFilter", "All") }, { "BundleTypes", {} }, { "AssetTypes", {} }, { "IncludeOffSale", false } }
			local Defaults = {}
			for _, Field in Fields do
				Defaults[Field[1]] = Field[2]
			end
			CatalogSearchParamsType = ParamsType("CatalogSearchParams", Defaults, nil, ParamsText("CatalogSearchParams", Fields))
		end
		return CatalogSearchParamsType.new(Initial)
	end,
}
-- PathWaypoint.new(Position, Action, Label): tostring "{0.333333343, 2, 3}, Jump, label" (Studio 0.744)
local PathWaypointType = ParamsType("PathWaypoint", {}, nil, function(Data)
	return {
		__tostring = function()
			local Position = Data.Position
			return `\{{string.format("%.9g", Position.X)}, {string.format("%.9g", Position.Y)}, {string.format("%.9g", Position.Z)}\}, {EnumItemName(Data.Action) or tostring(Data.Action)}, {Data.Label}`
		end,
	}
end)
local PathWaypointLibrary = {
	new = function(Position, Action, Label)
		return PathWaypointType.new({ Position = Position or vector.zero, Action = Action or GetEnumItem("PathWaypointAction", "Walk"), Label = Label or "" })
	end,
}
-- PhysicalProperties.new(Enum.Material.X) as in Roblox (a playing client, Studio 0.744: Wood is 0.35, 0.48, 0.2, 1, 1,
-- 0.25): Lune's constructor takes only its own EnumItems, so the material goes by name
do
	local LunePhysical = Datatypes.PhysicalProperties
	local LuneMaterials = Roblox.Enum.Material
	ConstructorOverrides.PhysicalProperties = {
		new = function(First, ...)
			if EnumItemType(First) == "Material" then
				local Name = EnumItemName(First)
				return PassResults("new", pcall(function()
					return LunePhysical.new(LuneMaterials[Name])
				end))
			end
			return PassResults("new", pcall(LunePhysical.new, First, ...))
		end,
	}
end

-- Path2DControlPoint: three UDim2 fields a script reads and writes, compared by value. Its fields live in
-- Path2DPoints, where the engine reads them (path2d.rs). Measured in a playing client (Studio 0.744) and in Studio
-- 0.747 (tools/parity/probes/path2d.luau): new takes no argument, a Position, or all three; a field takes a UDim2 only.
local Path2DPoints = setmetatable({}, { __mode = "k" })
local Path2DPointMeta = {
	__tostring = function(Self)
		local Fields = Path2DPoints[Self]
		return `Path2DControlPoint \{ Position = {Fields.Position}, LeftTangent = {Fields.LeftTangent}, RightTangent = {Fields.RightTangent} \}`
	end,
	__eq = function(Left, Right)
		local A, B = Path2DPoints[Left], Path2DPoints[Right]
		return A ~= nil and B ~= nil and A.Position == B.Position and A.LeftTangent == B.LeftTangent and A.RightTangent == B.RightTangent
	end,
}
local function Path2DPointIndex(Self, Key)
	local Value = Path2DPoints[Self][Key]
	if Value == nil then
		error(`'{Key}' is not a valid member of Path2DControlPoint`, 0)
	end
	return Value
end
local function Path2DPointNewIndex(Self, Key, Value)
	local Fields = Path2DPoints[Self]
	if Fields[Key] == nil then
		error(`{Key} is not a valid member of Path2DControlPoint`, 0)
	elseif Typeof(Value) ~= "UDim2" then
		error(`invalid argument #3 (UDim2 expected, got {Typeof(Value)})`, 2)
	end
	Fields[Key] = Value
end
function Path2DControlPoint.new(...)
	local Count = select("#", ...)
	local Fields = { Position = UDim2.new(), LeftTangent = UDim2.new(), RightTangent = UDim2.new() }
	if Count > 0 then
		local Position, LeftTangent, RightTangent = ...
		if Typeof(Position) ~= "UDim2" then
			error(`invalid argument #1 to 'new' (UDim2 expected, got {Typeof(Position)})`, 2)
		elseif Count ~= 1 and Count ~= 3 then
			error("Trying to create a Path2DControlPoint with wrong number of arguments", 0)
		end
		Fields.Position = Position
		if Count == 3 then
			if Typeof(LeftTangent) ~= "UDim2" then
				error(`invalid argument #2 to 'new' (UDim2 expected, got {Typeof(LeftTangent)})`, 2)
			elseif Typeof(RightTangent) ~= "UDim2" then
				error(`invalid argument #3 to 'new' (UDim2 expected, got {Typeof(RightTangent)})`, 2)
			end
			Fields.LeftTangent, Fields.RightTangent = LeftTangent, RightTangent
		end
	end
	local Point = Proxy("Path2DControlPoint", Path2DPointIndex, Path2DPointNewIndex, Path2DPointMeta)
	Path2DPoints[Point] = Fields
	return Point
end

return {
	Wrap = Wrap,
	WrapIndex = WrapIndex,
	WrapNewIndex = WrapNewIndex,
	Pass = Pass,
	Typeof = Typeof,
	Register = Register,
	TypeRegistry = Registry,
	Proxy = Proxy,
	ScriptLibrary = ScriptLibrary,
	Datatypes = Datatypes,
	Faces = Faces,
	Axes = Axes,
	Path2DControlPoint = Path2DControlPoint,
	Path2DPoints = Path2DPoints,
	RaycastParams = FilterParamsType("RaycastParams", { FilterDescendantsInstances = {}, IgnoreWater = false, RespectCanCollide = false, CollisionGroup = "Default" }, RaycastParamsText),
	OverlapParams = FilterParamsType("OverlapParams", { FilterDescendantsInstances = {}, MaxParts = 0, RespectCanCollide = false, CollisionGroup = "Default" }, OverlapParamsText),
	ParamsData = ParamsData,
	CatalogSearchParams = CatalogSearchParamsLibrary,
	PathWaypoint = PathWaypointLibrary,
	InertLibrary = InertLibrary,
	SharedTable = SharedTable,
	MakeStubTree = MakeStubTree,
	Utf8 = Utf8Table,
	Debug = DebugTable,
	Coroutine = Coroutine,
	Os = table.clone(os),
	Task = Task,
	LegacyWait = LegacyWait,
	Traceback = Traceback,
	VectorParts = VectorParts,
	GetEnumItem = GetEnumItem,
	Print = Print,
	Warn = Warn,
	Stringify = Stringify,
	Require = Require,
	Environment = Environment,
	CallBody = CallBody,
}
