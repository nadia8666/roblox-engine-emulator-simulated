--!nocheck
-- Rust engine: what a server (`--run-context server`) has that a client does not, loaded after
-- services.luau (lua/stubs.rs) in a server run only: in-memory DataStoreService, MemoryStoreService
-- and MessagingService, Player:Kick, and the server side of remotes. A run's stores start empty and
-- live in its own VM, so no two runs share data.
--
-- Every async method yields one frame (task.wait) and then acts at once: it reaches its store after
-- the yield, as Roblox's request reaches the service, and an UpdateAsync reads, transforms and writes
-- in one step (its transform function runs once and cannot yield). Values are kept as copies of what
-- a data store can hold (JSON-like data and buffers), so a read never returns the table that was
-- written. Limits, error codes and the request budgets follow Roblox's data store, memory store and
-- MessagingService documentation; requests are never throttled, and the error texts are not measured
-- (docs/engine.md, "Server run context").

local R = ...
local Method = R.Method
local Create = R.Create
local RawSet = R.RawSet
local Missing = R.Missing
local Pack, Unpack = table.pack, table.unpack

local Players = game:GetService("Players")

-- An error before the method yields names the script line that called it (the first frame that is not
-- the engine's), as the Rust behaviours' errors do; one after its yield has no position
local function Raise(Message)
	for Level = 2, 64 do
		local Source, Line = debug.info(Level, "sl")
		if Source == nil then
			break
		elseif Source ~= "[C]" and string.sub(Source, 1, 4) ~= "rur." then
			error(`{Source}:{Line}: {Message}`, 0)
		end
	end
	error(Message, 0)
end

-- (the checks below take At: true raises at the calling script's line, nil without a position)
local function Fail(Message, At)
	if At then
		Raise(Message)
	end
	error(Message, 0)
end

local At = true

local function NowMillis()
	return DateTime.now().UnixTimestampMillis
end

local function Now()
	return NowMillis() / 1000
end

local function IsEnum(Value, EnumName)
	return typeof(Value) == "EnumItem" and Value.EnumType == Enum[EnumName]
end

-- Data a store holds: a copy of Value and its length as JSON, or nil and the kind that cannot be stored

local function JsonStringLength(Text)
	local _, Escaped = string.gsub(Text, '[%c"\\]', "")
	return #Text + 2 + Escaped
end

local function Data(Value, Seen)
	local Kind = type(Value)
	if Kind == "nil" then
		return nil, 4
	elseif Kind == "boolean" then
		return Value, if Value then 4 else 5
	elseif Kind == "number" then
		return Value, #tostring(Value)
	elseif Kind == "string" then
		if utf8.len(Value) == nil then
			return nil, nil, "string"
		end
		return Value, JsonStringLength(Value)
	elseif Kind == "buffer" then
		local Length = buffer.len(Value)
		local Copy = buffer.create(Length)
		buffer.copy(Copy, 0, Value)
		return Copy, 36 + math.ceil(Length / 3) * 4
	elseif Kind ~= "table" then
		return nil, nil, typeof(Value)
	end
	if Seen[Value] then
		return nil, nil, "Dictionary"
	end
	Seen[Value] = true
	local Count, Length = 0, rawlen(Value)
	for _ in next, Value do
		Count += 1
	end
	local IsArray = Count > 0 and Count == Length
	if IsArray then
		for Index = 1, Length do
			if rawget(Value, Index) == nil then
				IsArray = false
				break
			end
		end
	end
	local Copy, Size = {}, 2
	if IsArray then
		for Index = 1, Length do
			local Item, ItemSize, Bad = Data(rawget(Value, Index), Seen)
			if Bad then
				return nil, nil, Bad
			end
			Copy[Index] = Item
			Size += ItemSize + 1
		end
	else
		for Key, Item in next, Value do
			if type(Key) ~= "string" then
				return nil, nil, if type(Key) == "number" then "Array" else "Dictionary"
			elseif utf8.len(Key) == nil then
				return nil, nil, "string"
			end
			local Copied, ItemSize, Bad = Data(Item, Seen)
			if Bad then
				return nil, nil, Bad
			end
			Copy[Key] = Copied
			Size += JsonStringLength(Key) + ItemSize + 2
		end
	end
	Seen[Value] = nil
	return Copy, Size
end

local function Clone(Value)
	if type(Value) == "table" then
		local Copy = {}
		for Key, Item in next, Value do
			Copy[Key] = Clone(Item)
		end
		return Copy
	elseif type(Value) == "buffer" then
		local Copy = buffer.create(buffer.len(Value))
		buffer.copy(Copy, 0, Value)
		return Copy
	end
	return Value
end

-- DataStoreService

local MAX_VALUE = 4194304
local MAX_NAME = 50

local Stores = {}
local Handles = setmetatable({}, { __mode = "k" })
local Infos = setmetatable({}, { __mode = "k" })
local OptionMetadata = setmetatable({}, { __mode = "k" })
local Serial = 0

local function Storable(Value, At)
	local Copy, Size, Bad = Data(Value, {})
	if Bad then
		Fail(`104: Cannot store {Bad} in data store. Data stores can only accept valid UTF-8 characters.`, At)
	elseif Size > MAX_VALUE then
		Fail("105: Serialized value exceeds 4MB limit.", At)
	end
	return Copy
end

local function CheckKey(Key, At)
	if type(Key) == "number" then
		Key = tostring(Key)
	end
	if Key == nil then
		Fail("Argument 1 missing or nil", At)
	elseif type(Key) ~= "string" then
		Fail(`Unable to cast {typeof(Key)} to string`, At)
	elseif #Key == 0 then
		Fail("101: Key name can't be empty.", At)
	elseif #Key > MAX_NAME then
		Fail("102: Key name exceeds the 50 character limit.", At)
	end
	return Key
end

local function CheckUserIds(UserIds, At)
	if UserIds == nil then
		return {}
	end
	if type(UserIds) ~= "table" then
		Fail("513: Attribute userId format is invalid.", At)
	end
	local Copy = {}
	for Index, UserId in UserIds do
		if type(Index) ~= "number" or type(UserId) ~= "number" then
			Fail("513: Attribute userId format is invalid.", At)
		end
		Copy[Index] = UserId
	end
	return Copy
end

local function CheckMetadata(Metadata, At)
	if Metadata == nil then
		return {}
	end
	if type(Metadata) ~= "table" then
		Fail("513: Attribute metadata format is invalid.", At)
	end
	local Copy, Size, Bad = Data(Metadata, {})
	if Bad or (next(Metadata) ~= nil and #Metadata > 0) then
		Fail("513: Attribute metadata format is invalid.", At)
	elseif Size > 300 then
		Fail("511: Metadata attribute size exceeds 300 limit.", At)
	end
	return Copy
end

local function Handle(Self, Member, At)
	local Store = Handles[Self]
	if not Store then
		Fail(`Expected ':' not '.' calling member function {Member}`, At)
	end
	return Store
end

local function StoreName(Name, Label, Default, At)
	if Name == nil and Default then
		return Default
	elseif type(Name) == "number" then
		Name = tostring(Name)
	end
	if type(Name) ~= "string" then
		Fail(if Name == nil then "Argument 1 missing or nil" else `Unable to cast {typeof(Name)} to string`, At)
	elseif #Name == 0 then
		Fail(`{Label} can't be empty.`, At)
	elseif #Name > MAX_NAME then
		Fail(`{Label} exceeds the 50 character limit.`, At)
	end
	return Name
end

local function GetStore(Kind, Class, Name, Scope)
	local Id = `{Kind}\0{Name}\0{Scope}`
	local Store = Stores[Id]
	if not Store then
		local Instance_ = Create(Class)
		RawSet(Instance_, "Name", Name, true)
		Store = { Kind = Kind, Name = Name, Scope = Scope, Keys = {}, Instance = Instance_ }
		Stores[Id] = Store
		Handles[Instance_] = Store
	end
	return Store.Instance
end

Method("DataStoreService", "GetDataStore", function(_, Name, Scope, Options)
	Name = StoreName(Name, "DataStore name", nil, At)
	Scope = StoreName(Scope, "Scope", "global", At)
	if Options ~= nil and typeof(Options) == "Instance" and Options:IsA("DataStoreOptions") and Options.AllScopes then
		Missing("DataStoreOptions", "AllScopes")
	end
	return GetStore("Standard", "DataStore", Name, Scope)
end)
Method("DataStoreService", "GetGlobalDataStore", function()
	return GetStore("Global", "GlobalDataStore", "GlobalDataStore", "global")
end)
Method("DataStoreService", "GetOrderedDataStore", function(_, Name, Scope)
	Name = StoreName(Name, "DataStore name", nil, At)
	Scope = StoreName(Scope, "Scope", "global", At)
	return GetStore("Ordered", "OrderedDataStore", Name, Scope)
end)

-- The server's budget a minute as Roblox's documentation gives it ({base, per player}): the legacy request
-- types' table and the Standard / Ordered ones'. Requests here are never throttled and spend nothing.
local Budgets = {
	GetAsync = { 60, 10 },
	SetIncrementAsync = { 60, 10 },
	UpdateAsync = { 60, 10 },
	GetSortedAsync = { 5, 2 },
	SetIncrementSortedAsync = { 30, 5 },
	OnUpdate = { 30, 5 },
	ListAsync = { 5, 2 },
	GetVersionAsync = { 5, 2 },
	RemoveVersionAsync = { 5, 2 },
	StandardRead = { 60, 40 },
	StandardWrite = { 60, 40 },
	StandardList = { 5, 2 },
	StandardRemove = { 60, 40 },
	OrderedRead = { 60, 40 },
	OrderedWrite = { 30, 5 },
	OrderedList = { 5, 2 },
	OrderedRemove = { 30, 5 },
}
Method("DataStoreService", "GetRequestBudgetForRequestType", function(_, RequestType)
	if RequestType == nil then
		Raise("Argument 1 missing or nil")
	elseif not IsEnum(RequestType, "DataStoreRequestType") then
		Raise("Unable to cast value to Enum.DataStoreRequestType")
	end
	local Budget = Budgets[RequestType.Name] or { 60, 10 }
	return Budget[1] + Budget[2] * #Players:GetPlayers()
end)

Method("DataStoreOptions", "SetExperimentalFeatures", function(_, Features)
	if type(Features) ~= "table" then
		Raise("Argument 1 missing or nil")
	end
end)
for _, Class in { "DataStoreSetOptions", "DataStoreIncrementOptions" } do
	Method(Class, "SetMetadata", function(Self, Attributes)
		OptionMetadata[Self] = CheckMetadata(Attributes, true)
	end)
	Method(Class, "GetMetadata", function(Self)
		return Clone(OptionMetadata[Self] or {})
	end)
end

local function MetadataOf(Options)
	if typeof(Options) == "Instance" and OptionMetadata[Options] then
		return Clone(OptionMetadata[Options])
	end
	return {}
end

local function Current(Record)
	local Last = Record and Record.Versions[#Record.Versions]
	if Last and not Last.Deleted then
		return Last
	end
	return nil
end

local function KeyInfo(Record, Version)
	local Info = Create("DataStoreKeyInfo")
	RawSet(Info, "CreatedTime", Record.Created, true)
	RawSet(Info, "UpdatedTime", Version.Created, true)
	RawSet(Info, "Version", Version.Version, true)
	Infos[Info] = Version
	return Info
end
Method("DataStoreKeyInfo", "GetMetadata", function(Self)
	local Version = Infos[Self]
	return Clone(if Version then Version.Metadata else {})
end)
Method("DataStoreKeyInfo", "GetUserIds", function(Self)
	local Version = Infos[Self]
	return Clone(if Version then Version.UserIds else {})
end)

local function Write(Store, Key, Value, UserIds, Metadata, Deleted)
	local Millis = NowMillis()
	local Record = Store.Keys[Key]
	if not Record or (Store.Kind == "Ordered" and Deleted) then
		Record = { Created = Millis, Versions = {} }
		Store.Keys[Key] = Record
	end
	Serial += 1
	local Version = {
		Version = string.format("%016X.%010d.%016X.01", Millis, Serial, Millis),
		Created = Millis,
		Value = Value,
		UserIds = UserIds or {},
		Metadata = Metadata or {},
		Deleted = Deleted == true,
	}
	if Store.Kind == "Ordered" then
		Record.Versions = { Version }
	else
		table.insert(Record.Versions, Version)
	end
	return Record, Version
end

local function OrderedValue(Value, At)
	if type(Value) ~= "number" then
		Fail(`104: Cannot store {typeof(Value)} in data store. Data stores can only accept valid UTF-8 characters.`, At)
	elseif Value % 1 ~= 0 then
		Fail("104: Cannot store double in data store. Data stores can only accept valid UTF-8 characters.", At)
	end
end

-- The transform function's results; it runs on a thread of its own so that a yield is refused
local function Transform(Function, Member, ...)
	local Thread = coroutine.create(Function)
	local Results = Pack(coroutine.resume(Thread, ...))
	if not Results[1] then
		error(Results[2], 0)
	end
	if coroutine.status(Thread) ~= "dead" then
		task.cancel(Thread)
		error(`{Member}: the transform function cannot yield`, 0)
	end
	return Pack(Unpack(Results, 2, Results.n))
end

Method("GlobalDataStore", "GetAsync", function(Self, Key, _Options)
	local Store = Handle(Self, "GetAsync", At)
	Key = CheckKey(Key, At)
	task.wait()
	local Record = Store.Keys[Key]
	local Version = Current(Record)
	if not Version then
		return nil
	elseif Store.Kind == "Ordered" then
		return Clone(Version.Value)
	end
	return Clone(Version.Value), KeyInfo(Record, Version)
end)

Method("GlobalDataStore", "SetAsync", function(Self, Key, Value, UserIds, Options)
	local Store = Handle(Self, "SetAsync", At)
	Key = CheckKey(Key, At)
	if Value == nil then
		Fail("Argument 2 missing or nil", At)
	end
	if Store.Kind == "Ordered" then
		OrderedValue(Value, At)
	end
	local Copy = Storable(Value, At)
	UserIds = CheckUserIds(UserIds, At)
	local Metadata = MetadataOf(Options)
	task.wait()
	local _, Version = Write(Store, Key, Copy, UserIds, Metadata)
	if Store.Kind ~= "Ordered" then
		return Version.Version
	end
	return nil
end)

Method("GlobalDataStore", "UpdateAsync", function(Self, Key, Function)
	local Store = Handle(Self, "UpdateAsync", At)
	Key = CheckKey(Key, At)
	if type(Function) ~= "function" then
		Fail(if Function == nil then "Argument 2 missing or nil" else "Unable to cast value to function", At)
	end
	task.wait()
	local Ordered = Store.Kind == "Ordered"
	local Record = Store.Keys[Key]
	local Version = Current(Record)
	local Old = if Version then Clone(Version.Value) else nil
	local Results = Transform(Function, "UpdateAsync", Old, if Version and not Ordered then KeyInfo(Record, Version) else nil)
	if Results[1] == nil then
		return nil
	end
	if Ordered then
		OrderedValue(Results[1], nil)
	end
	local Copy = Storable(Results[1], nil)
	local UserIds = CheckUserIds(Results[2], nil)
	local Metadata = CheckMetadata(Results[3], nil)
	local Written, New = Write(Store, Key, Copy, UserIds, Metadata)
	if Ordered then
		return Clone(Copy)
	end
	return Clone(Copy), KeyInfo(Written, New)
end)

Method("GlobalDataStore", "IncrementAsync", function(Self, Key, Delta, UserIds, Options)
	local Store = Handle(Self, "IncrementAsync", At)
	Key = CheckKey(Key, At)
	if Delta == nil then
		Delta = 1
	elseif type(Delta) ~= "number" then
		Fail("Unable to cast value to int", At)
	end
	Delta = if Delta < 0 then math.ceil(Delta) else math.floor(Delta)
	UserIds = CheckUserIds(UserIds, At)
	local Metadata = MetadataOf(Options)
	task.wait()
	local Version = Current(Store.Keys[Key])
	local Old = if Version then Version.Value else 0
	if type(Old) ~= "number" or Old % 1 ~= 0 then
		error("IncrementAsync: the stored value is not an integer", 0)
	end
	Write(Store, Key, Old + Delta, UserIds, Metadata)
	return Old + Delta
end)

Method("GlobalDataStore", "RemoveAsync", function(Self, Key)
	local Store = Handle(Self, "RemoveAsync", At)
	Key = CheckKey(Key, At)
	task.wait()
	local Record = Store.Keys[Key]
	local Version = Current(Record)
	if not Version then
		return nil
	elseif Store.Kind == "Ordered" then
		Store.Keys[Key] = nil
		return Clone(Version.Value)
	end
	Write(Store, Key, nil, Version.UserIds, Version.Metadata, true)
	return Clone(Version.Value), KeyInfo(Record, Version)
end)

local function CheckVersion(Version, At)
	if type(Version) ~= "string" or not string.match(Version, "^%x+%.%d+%.%x+%.%d+$") then
		Fail("The version is not valid.", At)
	end
end

Method("DataStore", "GetVersionAsync", function(Self, Key, Version)
	local Store = Handle(Self, "GetVersionAsync", At)
	Key = CheckKey(Key, At)
	CheckVersion(Version, At)
	task.wait()
	local Record = Store.Keys[Key]
	for _, Entry in if Record then Record.Versions else {} do
		if Entry.Version == Version and not Entry.Deleted then
			return Clone(Entry.Value), KeyInfo(Record, Entry)
		end
	end
	return nil
end)

Method("DataStore", "GetVersionAtTimeAsync", function(Self, Key, Timestamp)
	local Store = Handle(Self, "GetVersionAtTimeAsync", At)
	Key = CheckKey(Key, At)
	if type(Timestamp) ~= "number" or Timestamp < 0 or Timestamp > NowMillis() + 600000 then
		Fail("Timestamp must be positive and not more than ten minutes in the future.", At)
	end
	task.wait()
	local Record = Store.Keys[Key]
	local Found
	for _, Entry in if Record then Record.Versions else {} do
		if Entry.Created <= Timestamp then
			Found = Entry
		end
	end
	if not Found or Found.Deleted then
		return nil
	end
	return Clone(Found.Value), KeyInfo(Record, Found)
end)

Method("DataStore", "RemoveVersionAsync", function(Self, Key, Version)
	local Store = Handle(Self, "RemoveVersionAsync", At)
	Key = CheckKey(Key, At)
	CheckVersion(Version, At)
	task.wait()
	local Record = Store.Keys[Key]
	for Index, Entry in if Record then Record.Versions else {} do
		if Entry.Version == Version then
			table.remove(Record.Versions, Index)
			break
		end
	end
end)

-- Pages: the items cut into pages up front; IsFinished on the last
local PageLists = setmetatable({}, { __mode = "k" })

local function NewPages(Class, Items, PageSize, Cursor)
	local Pages = Create(Class)
	local List = {}
	for Index = 1, #Items, PageSize do
		table.insert(List, table.move(Items, Index, math.min(Index + PageSize - 1, #Items), 1, {}))
	end
	if #List == 0 then
		List[1] = {}
	end
	PageLists[Pages] = { List = List, Index = 1, Cursor = Cursor }
	RawSet(Pages, "IsFinished", #List <= 1, true)
	if Cursor then
		RawSet(Pages, "Cursor", Cursor(2, List) or "", true)
	end
	return Pages
end

local function PageCopy(Page)
	local Copy = {}
	for Index, Item in Page do
		Copy[Index] = if type(Item) == "table" then Clone(Item) else Item
	end
	return Copy
end

for _, Class in { "DataStorePages", "DataStoreVersionPages", "DataStoreKeyPages", "MemoryStoreHashMapPages" } do
	Method(Class, "GetCurrentPage", function(Self)
		local Pages = PageLists[Self]
		return if Pages then PageCopy(Pages.List[Pages.Index]) else {}
	end)
	Method(Class, "AdvanceToNextPageAsync", function(Self)
		local Pages = PageLists[Self]
		if not Pages or Pages.Index >= #Pages.List then
			Raise("No pages to advance to.")
		end
		task.wait()
		Pages.Index += 1
		RawSet(Self, "IsFinished", Pages.Index >= #Pages.List, true)
		if Pages.Cursor then
			RawSet(Self, "Cursor", Pages.Cursor(Pages.Index + 1, Pages.List) or "", true)
		end
	end)
end

local function PageSize(Size, Default, Maximum)
	if type(Size) ~= "number" or Size <= 0 then
		return Default
	end
	return math.min(math.floor(Size), Maximum)
end

Method("DataStore", "ListVersionsAsync", function(Self, Key, Direction, MinDate, MaxDate, Size)
	local Store = Handle(Self, "ListVersionsAsync", At)
	Key = CheckKey(Key, At)
	if Direction ~= nil and not IsEnum(Direction, "SortDirection") then
		Fail("Unable to cast value to Enum.SortDirection", At)
	end
	task.wait()
	local Items = {}
	local Record = Store.Keys[Key]
	for _, Entry in if Record then Record.Versions else {} do
		local After = type(MinDate) ~= "number" or MinDate <= 0 or Entry.Created >= MinDate
		local Before = type(MaxDate) ~= "number" or MaxDate <= 0 or Entry.Created <= MaxDate
		if After and Before then
			local Info = Create("DataStoreObjectVersionInfo")
			RawSet(Info, "CreatedTime", Entry.Created, true)
			RawSet(Info, "IsDeleted", Entry.Deleted, true)
			RawSet(Info, "Version", Entry.Version, true)
			table.insert(Items, Info)
		end
	end
	if Direction == Enum.SortDirection.Descending then
		local Reversed = {}
		for Index = #Items, 1, -1 do
			table.insert(Reversed, Items[Index])
		end
		Items = Reversed
	end
	return NewPages("DataStoreVersionPages", Items, PageSize(Size, 1024, 1024))
end)

Method("DataStore", "ListKeysAsync", function(Self, Prefix, Size, Cursor, ExcludeDeleted)
	local Store = Handle(Self, "ListKeysAsync", At)
	Prefix = if Prefix == nil then "" else tostring(Prefix)
	Cursor = if type(Cursor) == "string" then Cursor else ""
	task.wait()
	local Names = {}
	for Name, Record in Store.Keys do
		local Deleted = Current(Record) == nil
		if string.sub(Name, 1, #Prefix) == Prefix and Name >= Cursor and not (Deleted and ExcludeDeleted) then
			table.insert(Names, Name)
		end
	end
	table.sort(Names)
	local Items = {}
	for _, Name in Names do
		local Entry = Create("DataStoreKey")
		RawSet(Entry, "KeyName", Name, true)
		table.insert(Items, Entry)
	end
	return NewPages("DataStoreKeyPages", Items, PageSize(Size, 50, 1024), function(Index, List)
		local Page = List[Index]
		return Page and Page[1] and Page[1].KeyName
	end)
end)

Method("OrderedDataStore", "GetSortedAsync", function(Self, Ascending, Size, MinValue, MaxValue)
	local Store = Handle(Self, "GetSortedAsync", At)
	if type(Size) ~= "number" or Size < 1 or Size > 100 then
		Fail("106: PageSize must be within a predefined range.", At)
	elseif MinValue ~= nil and (type(MinValue) ~= "number" or MinValue % 1 ~= 0) then
		Fail("106: MinValue must be an integer.", At)
	elseif MaxValue ~= nil and (type(MaxValue) ~= "number" or MaxValue % 1 ~= 0) then
		Fail("106: MaxValue must be an integer.", At)
	elseif MinValue ~= nil and MaxValue ~= nil and MaxValue < MinValue then
		Fail("107: MaxValue must be greater than or equal to MinValue.", At)
	end
	task.wait()
	local Items = {}
	for Key, Record in Store.Keys do
		local Version = Current(Record)
		if Version and (MinValue == nil or Version.Value >= MinValue) and (MaxValue == nil or Version.Value <= MaxValue) then
			table.insert(Items, { key = Key, value = Version.Value })
		end
	end
	table.sort(Items, function(A, B)
		if A.value ~= B.value then
			return if Ascending then A.value < B.value else A.value > B.value
		end
		return A.key < B.key
	end)
	return NewPages("DataStorePages", Items, math.floor(Size))
end)

-- MemoryStoreService: sorted maps, hash maps and queues by name; items expire on the run's clock

local MAX_EXPIRATION = 3888000
local MAX_ITEM = 32 * 1024
local MemoryStores = { SortedMap = {}, HashMap = {}, Queue = {} }
local MemoryHandles = setmetatable({}, { __mode = "k" })

local function MemoryFail(Code, Message, At)
	Fail(`Code: {Code}, Error: {Message}`, At)
end

local function MemoryKey(Key, At)
	if type(Key) ~= "string" or #Key == 0 or #Key > 128 then
		MemoryFail("InvalidRequest", "The key must be a string of 1 to 128 characters.", At)
	end
	return Key
end

local function MemoryValue(Value, At)
	local Copy, Size, Bad = Data(Value, {})
	if Bad or Value == nil then
		MemoryFail("InvalidRequest", "Unable to convert value to json.", At)
	elseif Size > MAX_ITEM then
		MemoryFail("ItemValueSizeTooLarge", "Value size exceeds limit (32 KB).", At)
	end
	return Copy
end

local function Expiration(Seconds, At)
	if type(Seconds) ~= "number" or Seconds < 0 or Seconds > MAX_EXPIRATION then
		MemoryFail("InvalidExpirationTime", "The field 'expiration' time must be between 0 and 3,888,000.", At)
	end
	return Now() + Seconds
end

local function SortKey(Key, At)
	if Key ~= nil and type(Key) ~= "number" and type(Key) ~= "string" then
		MemoryFail("InvalidRequest", "Unable to convert sortKey to a valid number or string.", At)
	end
	return Key
end

local function Count(Value, At)
	if type(Value) ~= "number" or Value < 1 or Value > 200 or Value % 1 ~= 0 then
		MemoryFail("InvalidRequest", "The count must be an integer from 1 to 200.", At)
	end
	return Value
end

local function Live(Items, Key)
	local Item = Items[Key]
	if Item and Item.Expires <= Now() then
		Items[Key] = nil
		return nil
	end
	return Item
end

local function Memory(Kind, Class, Name, At, Extra)
	if type(Name) ~= "string" or #Name == 0 then
		Fail(if Name == nil then "Argument 1 missing or nil" else "The name must be a non-empty string.", At)
	end
	local Structure = MemoryStores[Kind][Name]
	if not Structure then
		Structure = { Name = Name, Items = {}, Serial = 0, Reads = {} }
		MemoryStores[Kind][Name] = Structure
	end
	local Instance_ = Create(Class)
	RawSet(Instance_, "Name", Name, true)
	MemoryHandles[Instance_] = { Structure = Structure, Extra = Extra }
	return Instance_
end

local function MemoryHandle(Self, Member, At)
	local Found = MemoryHandles[Self]
	if not Found then
		Fail(`Expected ':' not '.' calling member function {Member}`, At)
	end
	return Found.Structure, Found.Extra
end

Method("MemoryStoreService", "GetSortedMap", function(_, Name)
	return Memory("SortedMap", "MemoryStoreSortedMap", Name, true)
end)
Method("MemoryStoreService", "GetHashMap", function(_, Name)
	return Memory("HashMap", "MemoryStoreHashMap", Name, true)
end)
Method("MemoryStoreService", "GetQueue", function(_, Name, InvisibilityTimeout)
	local Timeout = if type(InvisibilityTimeout) == "number" then InvisibilityTimeout else 30
	return Memory("Queue", "MemoryStoreQueue", Name, true, Timeout)
end)

-- Sorted map order (Roblox's documentation): numeric sort keys, then string sort keys, then items without
-- one; equal sort keys by key. A bound without a key sorts before (lower) or after (upper) every key of its
-- sort key, and one without a sort key stands among the items that have none.
local function Rank(Sort)
	return if type(Sort) == "number" then 0 elseif type(Sort) == "string" then 1 else 2
end

local function Compare(SortA, KeyA, SortB, KeyB)
	local RankA, RankB = Rank(SortA), Rank(SortB)
	if RankA ~= RankB then
		return if RankA < RankB then -1 else 1
	elseif RankA < 2 and SortA ~= SortB then
		return if SortA < SortB then -1 else 1
	elseif KeyA == KeyB then
		return 0
	end
	return if KeyA < KeyB then -1 else 1
end

local function Bound(Value, Upper, At)
	if Value == nil then
		return nil
	elseif type(Value) ~= "table" or (Value.key ~= nil and type(Value.key) ~= "string") then
		MemoryFail("InvalidRequest", "A bound must be a table with a key and/or sortKey.", At)
	end
	SortKey(Value.sortKey, At)
	return { Sort = Value.sortKey, Key = if Value.key ~= nil then Value.key elseif Upper then false else nil }
end

local function OrderOf(Item, Key, Bound_)
	local KeyB = Bound_.Key
	if KeyB == nil or KeyB == false then
		local Order = Compare(Item.Sort, Key, Bound_.Sort, Key)
		if Order ~= 0 then
			return Order
		end
		return if KeyB == false then -1 else 1
	end
	return Compare(Item.Sort, Key, Bound_.Sort, KeyB)
end

Method("MemoryStoreSortedMap", "SetAsync", function(Self, Key, Value, Seconds, Sort)
	local Map = MemoryHandle(Self, "SetAsync", At)
	Key = MemoryKey(Key, At)
	local Copy = MemoryValue(Value, At)
	local Expires = Expiration(Seconds, At)
	Sort = SortKey(Sort, At)
	task.wait()
	local Existed = Live(Map.Items, Key) ~= nil
	Map.Items[Key] = { Value = Copy, Sort = Sort, Expires = Expires }
	return Existed
end)
Method("MemoryStoreSortedMap", "GetAsync", function(Self, Key)
	local Map = MemoryHandle(Self, "GetAsync", At)
	Key = MemoryKey(Key, At)
	task.wait()
	local Item = Live(Map.Items, Key)
	if not Item then
		return nil
	end
	return Clone(Item.Value), Item.Sort
end)
Method("MemoryStoreSortedMap", "RemoveAsync", function(Self, Key)
	local Map = MemoryHandle(Self, "RemoveAsync", At)
	Key = MemoryKey(Key, At)
	task.wait()
	Map.Items[Key] = nil
end)
Method("MemoryStoreSortedMap", "UpdateAsync", function(Self, Key, Function, Seconds)
	local Map = MemoryHandle(Self, "UpdateAsync", At)
	Key = MemoryKey(Key, At)
	if type(Function) ~= "function" then
		Fail("Argument 2 missing or nil", At)
	end
	local Expires = Expiration(Seconds, At)
	task.wait()
	local Item = Live(Map.Items, Key)
	local Results = Transform(Function, "UpdateAsync", if Item then Clone(Item.Value) else nil, if Item then Item.Sort else nil)
	if Results[1] == nil then
		return nil
	end
	local Copy = MemoryValue(Results[1], nil)
	local Sort = SortKey(Results[2], nil)
	Map.Items[Key] = { Value = Copy, Sort = Sort, Expires = Expires }
	return Clone(Copy), Sort
end)
Method("MemoryStoreSortedMap", "GetRangeAsync", function(Self, Direction, Limit, Lower, Upper)
	local Map = MemoryHandle(Self, "GetRangeAsync", At)
	if not IsEnum(Direction, "SortDirection") then
		Fail(if Direction == nil then "Argument 1 missing or nil" else "Unable to cast value to Enum.SortDirection", At)
	end
	Limit = Count(Limit, At)
	Lower = Bound(Lower, false, At)
	Upper = Bound(Upper, true, At)
	task.wait()
	local Keys = {}
	for Key in Map.Items do
		local Item = Live(Map.Items, Key)
		if Item and (not Lower or OrderOf(Item, Key, Lower) > 0) and (not Upper or OrderOf(Item, Key, Upper) < 0) then
			table.insert(Keys, Key)
		end
	end
	table.sort(Keys, function(A, B)
		return Compare(Map.Items[A].Sort, A, Map.Items[B].Sort, B) < 0
	end)
	local Items = {}
	local Descending = Direction == Enum.SortDirection.Descending
	for Index = 1, math.min(Limit, #Keys) do
		local Key = Keys[if Descending then #Keys + 1 - Index else Index]
		local Item = Map.Items[Key]
		table.insert(Items, { key = Key, value = Clone(Item.Value), sortKey = Item.Sort })
	end
	return Items
end)
Method("MemoryStoreSortedMap", "GetSizeAsync", function(Self)
	local Map = MemoryHandle(Self, "GetSizeAsync", true)
	task.wait()
	local Size = 0
	for Key in Map.Items do
		if Live(Map.Items, Key) then
			Size += 1
		end
	end
	return Size
end)

Method("MemoryStoreHashMap", "SetAsync", function(Self, Key, Value, Seconds)
	local Map = MemoryHandle(Self, "SetAsync", At)
	Key = MemoryKey(Key, At)
	local Copy = MemoryValue(Value, At)
	local Expires = Expiration(Seconds, At)
	task.wait()
	Map.Items[Key] = { Value = Copy, Expires = Expires }
	return true
end)
Method("MemoryStoreHashMap", "GetAsync", function(Self, Key)
	local Map = MemoryHandle(Self, "GetAsync", At)
	Key = MemoryKey(Key, At)
	task.wait()
	local Item = Live(Map.Items, Key)
	return if Item then Clone(Item.Value) else nil
end)
Method("MemoryStoreHashMap", "RemoveAsync", function(Self, Key)
	local Map = MemoryHandle(Self, "RemoveAsync", At)
	Key = MemoryKey(Key, At)
	task.wait()
	Map.Items[Key] = nil
end)
Method("MemoryStoreHashMap", "UpdateAsync", function(Self, Key, Function, Seconds)
	local Map = MemoryHandle(Self, "UpdateAsync", At)
	Key = MemoryKey(Key, At)
	if type(Function) ~= "function" then
		Fail("Argument 2 missing or nil", At)
	end
	local Expires = Expiration(Seconds, At)
	task.wait()
	local Item = Live(Map.Items, Key)
	local Results = Transform(Function, "UpdateAsync", if Item then Clone(Item.Value) else nil)
	if Results[1] == nil then
		return nil
	end
	local Copy = MemoryValue(Results[1], nil)
	Map.Items[Key] = { Value = Copy, Expires = Expires }
	return Clone(Copy)
end)
Method("MemoryStoreHashMap", "ListItemsAsync", function(Self, Limit)
	local Map = MemoryHandle(Self, "ListItemsAsync", At)
	Limit = Count(Limit, At)
	task.wait()
	local Keys = {}
	for Key in Map.Items do
		if Live(Map.Items, Key) then
			table.insert(Keys, Key)
		end
	end
	table.sort(Keys)
	local Items = {}
	for _, Key in Keys do
		table.insert(Items, { key = Key, value = Clone(Map.Items[Key].Value) })
	end
	return NewPages("MemoryStoreHashMapPages", Items, Limit)
end)

-- Queues: higher priority first, then in the order added; a read hides its items for the queue's
-- invisibility timeout until RemoveAsync takes them; ReadAsync tries again every 2 s while it waits
local function Visible(Queue, Now_)
	local Found = {}
	for Index = #Queue.Items, 1, -1 do
		if Queue.Items[Index].Expires <= Now_ then
			table.remove(Queue.Items, Index)
		end
	end
	for _, Item in Queue.Items do
		if Item.Hidden <= Now_ then
			table.insert(Found, Item)
		end
	end
	table.sort(Found, function(A, B)
		if A.Priority ~= B.Priority then
			return A.Priority > B.Priority
		end
		return A.Serial < B.Serial
	end)
	return Found
end

Method("MemoryStoreQueue", "AddAsync", function(Self, Value, Seconds, Priority)
	local Queue = MemoryHandle(Self, "AddAsync", At)
	local Copy = MemoryValue(Value, At)
	local Expires = Expiration(Seconds, At)
	if Priority ~= nil and type(Priority) ~= "number" then
		Fail("Unable to cast value to double", At)
	end
	task.wait()
	Queue.Serial += 1
	table.insert(Queue.Items, { Value = Copy, Expires = Expires, Priority = Priority or 0, Serial = Queue.Serial, Hidden = 0 })
end)
Method("MemoryStoreQueue", "ReadAsync", function(Self, Limit, AllOrNothing, WaitTimeout)
	local Queue, Invisibility = MemoryHandle(Self, "ReadAsync", At)
	Limit = Count(Limit, At)
	WaitTimeout = if type(WaitTimeout) == "number" then WaitTimeout else -1
	local Started = Now()
	task.wait()
	while true do
		local Found = Visible(Queue, Now())
		if #Found >= Limit or (#Found > 0 and not AllOrNothing) then
			local Values, Taken = {}, {}
			for Index = 1, math.min(Limit, #Found) do
				local Item = Found[Index]
				Item.Hidden = Now() + Invisibility
				table.insert(Taken, Item)
				table.insert(Values, Clone(Item.Value))
			end
			Queue.Serial += 1
			local Id = string.format("%016X", Queue.Serial)
			Queue.Reads[Id] = Taken
			return Values, Id
		elseif WaitTimeout >= 0 and Now() - Started >= WaitTimeout then
			return {}, nil
		end
		task.wait(if WaitTimeout >= 0 then math.min(2, math.max(WaitTimeout - (Now() - Started), 0)) else 2)
	end
end)
Method("MemoryStoreQueue", "RemoveAsync", function(Self, Id)
	local Queue = MemoryHandle(Self, "RemoveAsync", At)
	if type(Id) ~= "string" then
		Fail("Argument 1 missing or nil", At)
	end
	task.wait()
	local Taken = Queue.Reads[Id]
	Queue.Reads[Id] = nil
	for _, Item in Taken or {} do
		local Index = table.find(Queue.Items, Item)
		if Index then
			table.remove(Queue.Items, Index)
		end
	end
end)
Method("MemoryStoreQueue", "GetSizeAsync", function(Self, ExcludeInvisible)
	local Queue = MemoryHandle(Self, "GetSizeAsync", true)
	task.wait()
	local Now_ = Now()
	local Visible_ = Visible(Queue, Now_)
	return if ExcludeInvisible then #Visible_ else #Queue.Items
end)

-- MessagingService: a topic is a BindableEvent of its own; PublishAsync delivers a copy of the message
-- ({Data, Sent}) to this server's subscribers (the only server there is) a frame after it is called

local Topics = {}

local function Topic(Name, At)
	if type(Name) ~= "string" then
		Fail(if Name == nil then "Argument 1 missing or nil" else `Unable to cast {typeof(Name)} to string`, At)
	elseif #Name == 0 or #Name > 80 then
		Fail("The topic name must be 1 to 80 characters long.", At)
	end
	return Name
end

Method("MessagingService", "SubscribeAsync", function(_, Name, Callback)
	Name = Topic(Name, At)
	if type(Callback) ~= "function" then
		Fail("Argument 2 missing or nil", At)
	end
	task.wait()
	local Event = Topics[Name]
	if not Event then
		Event = Create("BindableEvent")
		Topics[Name] = Event
	end
	return Event.Event:Connect(Callback)
end)
Method("MessagingService", "PublishAsync", function(_, Name, Message)
	Name = Topic(Name, At)
	local Copy, Size, Bad = Data(Message, {})
	if Bad then
		Fail(`PublishAsync: cannot send {Bad}`, At)
	elseif Size > 1024 then
		Fail("PublishAsync: the message is larger than 1 kB", At)
	end
	task.wait()
	local Event = Topics[Name]
	if Event then
		Event:Fire({ Data = Copy, Sent = Now() })
	end
end)

-- Player:Kick: the player leaves (PlayerRemoving) at the next resumption point

Method("Player", "Kick", function(Self, Message)
	if Message ~= nil and type(Message) ~= "string" then
		Raise(`Unable to cast {typeof(Message)} to string`)
	end
	task.defer(function()
		if Self.Parent == Players then
			R.SetParent(Self, nil)
		end
	end)
end)

-- Player:GetJoinData: the join data a harness gave the player (Context.SetJoinData, through R.SetJoinData)
-- before it joined, as a teleport would have carried it (TeleportData, Members, SourcePlaceId, ...). Kept
-- as a copy of JSON-like data, as a teleport serialises it, and every call returns a fresh copy; a player
-- given none gets an empty table.

local JoinData = setmetatable({}, { __mode = "k" })

Method("Player", "GetJoinData", function(Self)
	local Given = JoinData[Self]
	return if Given then Clone(Given) else {}
end)

R.SetJoinData = function(Player, Given)
	if typeof(Player) ~= "Instance" or not Player:IsA("Player") then
		error("SetJoinData: a Player expected", 2)
	end
	if Given == nil then
		JoinData[Player] = nil
		return
	end
	local Copy, _, Bad = Data(Given, {})
	if type(Copy) ~= "table" or Bad then
		error(`SetJoinData: join data is a table of JSON-like data{if Bad then ` (not {Bad})` else ""}`, 2)
	end
	JoinData[Player] = Copy
end

-- Players:CreateHumanoidModelFromDescription(Async) with Enum.HumanoidRigType.R6: offline there is no
-- avatar to fetch, so a server builds the default blocky R6 rig Roblox's documentation describes (a
-- HumanoidRootPart, Torso, Head and four limbs at their standard sizes, joined by the standard Motor6Ds,
-- named after nothing: the caller names it); server code reads its parts (PrimaryPart, the root). R15
-- and the client keep services.luau's bare Model with a Humanoid.

local R6_PARTS = {
	{ "HumanoidRootPart", Vector3.new(2, 2, 1), CFrame.new(0, 3, 0) },
	{ "Torso", Vector3.new(2, 2, 1), CFrame.new(0, 3, 0) },
	{ "Head", Vector3.new(2, 1, 1), CFrame.new(0, 4.5, 0) },
	{ "Left Arm", Vector3.new(1, 2, 1), CFrame.new(-1.5, 3, 0) },
	{ "Right Arm", Vector3.new(1, 2, 1), CFrame.new(1.5, 3, 0) },
	{ "Left Leg", Vector3.new(1, 2, 1), CFrame.new(-0.5, 1, 0) },
	{ "Right Leg", Vector3.new(1, 2, 1), CFrame.new(0.5, 1, 0) },
}
local R6_JOINTS = {
	{ "RootJoint", "HumanoidRootPart", "Torso", CFrame.new(0, 0, 0, -1, 0, 0, 0, 0, 1, 0, 1, -0), CFrame.new(0, 0, 0, -1, 0, 0, 0, 0, 1, 0, 1, -0) },
	{ "Neck", "Torso", "Head", CFrame.new(0, 1, 0, -1, 0, 0, 0, 0, 1, 0, 1, -0), CFrame.new(0, -0.5, 0, -1, 0, 0, 0, 0, 1, 0, 1, -0) },
	{ "Left Shoulder", "Torso", "Left Arm", CFrame.new(-1, 0.5, 0, -0, -0, -1, 0, 1, 0, 1, 0, 0), CFrame.new(0.5, 0.5, 0, -0, -0, -1, 0, 1, 0, 1, 0, 0) },
	{ "Right Shoulder", "Torso", "Right Arm", CFrame.new(1, 0.5, 0, 0, 0, 1, 0, 1, 0, -1, -0, -0), CFrame.new(-0.5, 0.5, 0, 0, 0, 1, 0, 1, 0, -1, -0, -0) },
	{ "Left Hip", "Torso", "Left Leg", CFrame.new(-1, -1, 0, -0, -0, -1, 0, 1, 0, 1, 0, 0), CFrame.new(-0.5, 1, 0, -0, -0, -1, 0, 1, 0, 1, 0, 0) },
	{ "Right Hip", "Torso", "Right Leg", CFrame.new(1, -1, 0, 0, 0, 1, 0, 1, 0, -1, -0, -0), CFrame.new(0.5, 1, 0, 0, 0, 1, 0, 1, 0, -1, -0, -0) },
}

local function R6Rig()
	local Model = Create("Model")
	local Parts = {}
	for _, Spec in R6_PARTS do
		local Part = Create("Part")
		Part.Name = Spec[1]
		Part.Size = Spec[2]
		Part.CFrame = Spec[3]
		if Spec[1] == "HumanoidRootPart" then
			Part.Transparency = 1
		end
		R.SetParent(Part, Model)
		Parts[Spec[1]] = Part
	end
	for _, Spec in R6_JOINTS do
		local Joint = Create("Motor6D")
		Joint.Name = Spec[1]
		Joint.Part0 = Parts[Spec[2]]
		Joint.Part1 = Parts[Spec[3]]
		Joint.C0 = Spec[4]
		Joint.C1 = Spec[5]
		R.SetParent(Joint, if Spec[1] == "RootJoint" then Parts.HumanoidRootPart else Parts.Torso)
	end
	local Humanoid = Create("Humanoid")
	Humanoid.RigType = Enum.HumanoidRigType.R6
	R.SetParent(Humanoid, Model)
	Model.PrimaryPart = Parts.HumanoidRootPart
	return Model
end

for _, Name in { "CreateHumanoidModelFromDescription", "CreateHumanoidModelFromDescriptionAsync" } do
	Method("Players", Name, function(_, Description, RigType)
		if typeof(Description) ~= "Instance" or not Description:IsA("HumanoidDescription") then
			Raise(`Unable to cast {typeof(Description)} to HumanoidDescription`)
		end
		if RigType == Enum.HumanoidRigType.R6 then
			return R6Rig()
		end
		local Model = Create("Model")
		R.SetParent(Create("Humanoid"), Model)
		return Model
	end)
end

-- `--loopback`: Roblox's server loads a joining player's character itself (CharacterAutoLoads); offline there is no avatar to
-- fetch, so LoadCharacter builds the default R6 rig above at the first SpawnLocation (the origin when the place has none),
-- puts StarterCharacterScripts in it, parents it to Workspace and sets Player.Character (which fires CharacterAdded), then
-- respawns the player Players.RespawnTime seconds after the humanoid dies, as the engine does.

if R.Loopback then
	local function Spawn(Player)
		local Old = Player.Character
		if Old then
			Old:Destroy()
		end
		local Model = R6Rig()
		Model.Name = Player.Name
		local Place = game:GetService("Workspace")
		local SpawnPoint = Place:FindFirstChildWhichIsA("SpawnLocation", true)
		Model:PivotTo(if SpawnPoint then SpawnPoint.CFrame + Vector3.new(0, 3, 0) else CFrame.new(0, 10, 0))
		local StarterScripts = game:GetService("StarterPlayer"):FindFirstChild("StarterCharacterScripts")
		if StarterScripts then
			for _, Child in StarterScripts:GetChildren() do
				Child:Clone().Parent = Model
			end
		end
		Model.Parent = Place
		Player.Character = Model
		Model:FindFirstChildOfClass("Humanoid").Died:Connect(function()
			task.delay(Players.RespawnTime, function()
				if Player.Parent == Players and Player.Character == Model then
					Spawn(Player)
				end
			end)
		end)
	end
	for _, Name in { "LoadCharacter", "LoadCharacterAsync" } do
		Method("Player", Name, function(Self)
			Spawn(Self)
		end)
	end
end

-- InsertService:LoadAsset: a run is offline, so every request fails as a failed load does on Roblox (the
-- documentation's failure mode: it raises), after the request's frame. Not emulating it returned nil,
-- which no Roblox call returns, so callers that pcall it read the nil as an asset.

Method("InsertService", "LoadAsset", function(_, AssetId)
	if type(AssetId) ~= "number" then
		Raise(`Unable to cast {typeof(AssetId)} to int64`)
	end
	task.wait()
	error(`InsertService:LoadAsset: asset {AssetId} cannot be loaded offline`, 0)
end)

-- InsertService:LoadAssetVersion and AssetService:LoadAssetAsync fail the same way, after the request's
-- frame: a caller that retries a failed load (a live server's asset bundle loader) yields between tries
-- as it does on Roblox, instead of failing at once and retrying in the same frame until the C stack
-- runs out.

Method("InsertService", "LoadAssetVersion", function(_, AssetVersionId)
	if type(AssetVersionId) ~= "number" then
		Raise(`Unable to cast {typeof(AssetVersionId)} to int64`)
	end
	task.wait()
	error(`InsertService:LoadAssetVersion: asset version {AssetVersionId} cannot be loaded offline`, 0)
end)

Method("AssetService", "LoadAssetAsync", function(_, AssetId)
	if type(AssetId) ~= "number" then
		Raise(`Unable to cast {typeof(AssetId)} to int64`)
	end
	task.wait()
	error(`AssetService:LoadAssetAsync: asset {AssetId} cannot be loaded offline`, 0)
end)

-- Remotes, the server's side: a client never listens and never answers here. FireClient / FireAllClients
-- send nothing; InvokeClient waits as for a client that never returns, until the player leaves, and then
-- raises; the client-only calls raise as Roblox's do.

local function IsPlayer(Value)
	return typeof(Value) == "Instance" and Value:IsA("Player")
end

if not R.Loopback then
for _, Class in { "RemoteEvent", "UnreliableRemoteEvent" } do
	Method(Class, "FireClient", function(_, Player)
		if not IsPlayer(Player) then
			Raise("FireClient: player argument must be a Player object")
		end
	end)
	Method(Class, "FireAllClients", function() end)
	Method(Class, "FireServer", function()
		Raise("FireServer can only be called from the client")
	end)
end
Method("RemoteFunction", "InvokeServer", function()
	Raise("InvokeServer can only be called from the client")
end)
Method("RemoteFunction", "InvokeClient", function(Self, Player)
	if not IsPlayer(Player) then
		Raise("InvokeClient: player argument must be a Player object")
	end
	if Player.Parent == Players then
		local Thread = coroutine.running()
		local Connection
		Connection = Players.PlayerRemoving:Connect(function(Leaving)
			if Leaving == Player then
				Connection:Disconnect()
				task.spawn(Thread)
			end
		end)
		coroutine.yield()
	end
	error(`InvokeClient: {Player.Name} left the game before {Self:GetFullName()} returned`, 0)
end)
else
	-- `--loopback`: one VM plays both sides, so an invoke reaches the callback the other side set (waiting, as a real
	-- invoke does, until one is set); its arguments are not copied
	local function WaitForCallback(Self, Name)
		local Callback = R.GetCallback(Self, Name)
		while not Callback do
			task.wait()
			Callback = R.GetCallback(Self, Name)
		end
		return Callback
	end
	Method("RemoteFunction", "InvokeServer", function(Self, ...)
		return WaitForCallback(Self, "OnServerInvoke")(Players.LocalPlayer, ...)
	end)
	Method("RemoteFunction", "InvokeClient", function(Self, Player, ...)
		if not IsPlayer(Player) then
			Raise("InvokeClient: player argument must be a Player object")
		end
		return WaitForCallback(Self, "OnClientInvoke")(...)
	end)
end

return true
