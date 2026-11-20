-- parity: identity=4 flag=FFlagEnableLoadModule
-- What debug.loadmodule gives a script: a ModuleScript's source compiled and not run, the chunk's name, its environment,
-- and what a source that does not compile answers. Writing Source needs identity 4 in a real engine, and loadmodule works
-- only where FFlagEnableLoadModule is on (ree: --load-module); core_services.luau has its answer where it is off.
local Folder = Instance.new("Folder")
Folder.Name = "ParityLoad"
Folder.Parent = game:GetService("ReplicatedStorage")
local function Make(Name, Source, Class)
	local Module = Instance.new(Class or "ModuleScript")
	Module.Name = Name
	Module.Source = Source
	Module.Parent = Folder
	return Module
end
local function Info(Function)
	return tostring(debug.info(Function, "s")) .. " n=[" .. tostring(debug.info(Function, "n")) .. "] l=" .. tostring(debug.info(Function, "l"))
end
local function Load(Label, ...)
	local Results = table.pack(pcall(debug.loadmodule, ...))
	if not Results[1] then
		print(Label .. " raised " .. P.err(Results[2]))
		return nil
	end
	local Function = Results[2]
	print(Label .. " " .. type(Function) .. " " .. P.err(tostring(Results[3])) .. " " .. type(Results[4]) .. " results=" .. (Results.n - 1)
		.. (if type(Function) == "function" then " " .. Info(Function) else ""))
	return if type(Function) == "function" then Function else nil, Results[4]
end
local function Call(Label, Function, ...)
	if Function then
		print(Label .. " call " .. P.try(Function, ...))
	end
end
print("type " .. type(debug.loadmodule))
print("library-env require " .. P.try(function()
	return type(getfenv(debug.loadmodule).require), rawequal(getfenv(debug.loadmodule), getfenv(1)), rawequal(getfenv(debug.loadmodule), getfenv(print))
end))

Call("plain", Load("plain", Make("Plain", "return 1, 'two', ...")), "three")
Call("empty", Load("empty", Make("Empty", "")))
Call("fresh", Load("fresh", Instance.new("ModuleScript")))
Call("fail", Load("fail", Make("Fail", "local A = 1\nerror('boom')")))
Call("index", Load("index", Make("Index", "local Missing = nil\nreturn Missing.Field")))
Call("traceback", Load("traceback", Make("Trace", "return (string.gsub(debug.traceback('t'), '\\n.*', ''))")))
Call("odd-name", Load("odd-name", Make("my module [1]", "error('boom')")))
Call("directives", Load("directives", Make("Directives", "--!strict\n--!native\n--!optimize 2\nreturn 1")))
Call("no-return", Load("no-return", Make("NoReturn", "local A = 1")))
Call("yields", Load("yields", Make("Yields", "task.wait() return 'waited'")))
Load("syntax", Make("Syntax", "local = 1"))
Load("syntax-lines", Make("SyntaxLines", "local A = 1\nlocal B = (\nreturn"))
Load("script", Make("AScript", "return 1", "Script"))
Load("local-script", Make("ALocalScript", "return 1", "LocalScript"))
Load("folder", Folder)
Load("no-argument")
Load("nil", nil)
Load("string", "return 1")
Load("number", 12)
Load("extra-arguments", Make("Extra", "return 1"), "more", 3)

-- Not run at load, and each call runs the source again
local Counter = Make("Counter", "_G.ParityLoads = (_G.ParityLoads or 0) + 1 return { Count = _G.ParityLoads }")
_G.ParityLoads = nil
local Count = Load("not-run", Counter)
print("not-run before " .. tostring(_G.ParityLoads))
if Count then
	local First, Second = Count(), Count()
	print("not-run calls " .. tostring(First.Count) .. " " .. tostring(Second.Count) .. " same-table " .. tostring(rawequal(First, Second)))
	local Required = require(Counter)
	print("not-run require " .. tostring(Required.Count) .. " again " .. tostring(require(Counter).Count) .. " then-call " .. tostring(Count().Count))
end
_G.ParityLoads = nil

-- The source is read at each load
local Changing = Make("Changing", "return 'first'")
local Before = Load("changing", Changing)
Changing.Source = "return 'second'"
local After = Load("changing", Changing)
Call("changing before", Before)
Call("changing after", After)
Changing.Name = "Renamed"
print("changing renamed " .. (if After then Info(After) else "nil"))
Call("renamed", Load("renamed", Changing))
local Loose = Instance.new("ModuleScript")
Loose.Name = "Loose"
Loose.Source = "error('boom')"
Call("loose", Load("loose", Loose))

-- The environment
local Reader = Make("Reader", "return { getfenv(1), script, getfenv(0), _G, shared, require, game }")
local Read, Cleanup = Load("env", Reader)
if Read then
	local Env, Script, Thread, Globals, Shared, Require, Game = table.unpack(Read(), 1, 7)
	local Keys = {}
	for Key in next, Env do
		table.insert(Keys, tostring(Key))
	end
	table.sort(Keys)
	local Meta = getmetatable(Env)
	print("env keys [" .. table.concat(Keys, ",") .. "] metatable " .. type(Meta) .. " index " .. type(type(Meta) == "table" and Meta.__index or nil)
		.. " frozen " .. tostring(table.isfrozen(Env)))
	print("env is-getfenv(f) " .. tostring(rawequal(Env, getfenv(Read))) .. " is-caller's " .. tostring(rawequal(Env, getfenv(1)))
		.. " script " .. tostring(rawequal(Script, Reader)) .. " raw-script " .. tostring(rawequal(rawget(Env, "script"), Reader))
		.. " thread-env-is-caller's " .. tostring(rawequal(Thread, getfenv(0)))
		.. " _G " .. tostring(rawequal(Globals, _G)) .. " shared " .. tostring(rawequal(Shared, shared))
		.. " require " .. tostring(rawequal(Require, require)) .. " game " .. tostring(rawequal(Game, game)))
	local Twin = Load("env-twin", Reader)
	print("env twin same-function " .. tostring(rawequal(Twin, Read)) .. " same-env " .. tostring(rawequal(getfenv(Twin), Env)))
	local RequiredEnv = require(Reader)
	print("env require's same-env " .. tostring(rawequal(RequiredEnv[1], Env)))
	print("env require " .. P.try(function()
		return Require(Make("Dependency", "return 'dependency ' .. script.Name"))
	end))
	print("cleanup " .. P.try(function()
		return Cleanup()
	end))
	print("cleanup again " .. P.try(function()
		return Cleanup()
	end))
	print("cleanup then-call " .. P.try(function()
		return type((Read()))
	end))
end
local Fenced = Load("setfenv", Make("Fenced", "return Marker, type(print), script"))
if Fenced then
	local Default = getfenv(Fenced)
	print("setfenv " .. P.try(function()
		return setfenv(Fenced, { Marker = "replaced", type = type }) == Fenced
	end))
	Call("setfenv", Fenced)
	local Layered = setmetatable({ Marker = "layered", script = "mocked" }, { __index = Default })
	setfenv(Fenced, Layered)
	Call("setfenv layered", Fenced)
	print("setfenv getfenv " .. tostring(rawequal(getfenv(Fenced), Layered)))
	local Other = Load("setfenv-other", Make("Fenced", "return Marker, type(print), script.Name"))
	Call("setfenv other-load", Other)
end
Folder:Destroy()
