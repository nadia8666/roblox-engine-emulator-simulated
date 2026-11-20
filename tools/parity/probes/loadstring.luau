-- What loadstring gives a script: the chunk's name, its environment, what a text that does not compile answers, and what
-- arguments it takes. A client has no loadstring; a server has it where ServerScriptService.LoadStringEnabled is on.
local function Info(Function)
	return tostring(debug.info(Function, "s")) .. " n=[" .. tostring(debug.info(Function, "n")) .. "] l=" .. tostring(debug.info(Function, "l"))
end
local function Load(Label, ...)
	local Results = table.pack(pcall(loadstring, ...))
	if not Results[1] then
		print(Label .. " raised " .. P.err(Results[2]))
		return nil
	end
	local Function = Results[2]
	print(Label .. " " .. type(Function) .. " " .. P.err(tostring(Results[3])) .. " results=" .. (Results.n - 1)
		.. (if type(Function) == "function" then " " .. Info(Function) else ""))
	return if type(Function) == "function" then Function else nil
end
local function Call(Label, Function, ...)
	if Function then
		print(Label .. " call " .. P.try(Function, ...))
	end
end
print("type " .. type(loadstring))

Call("plain", Load("plain", "return 1, 'two', ..."), "three")
Call("named", Load("named", "error('boom')", "Name"))
Call("named-equals", Load("named-equals", "error('boom')", "=Name"))
Call("named-at", Load("named-at", "error('boom')", "@Name"))
Call("named-empty", Load("named-empty", "error('boom')", ""))
Call("named-number", Load("named-number", "error('boom')", 12))
Call("unnamed", Load("unnamed", "error('boom')"))
Call("unnamed-lines", Load("unnamed-lines", "local A = 1\nerror('boom')"))
Call("unnamed-long", Load("unnamed-long", "local Long = '" .. string.rep("x", 120) .. "' error('boom')"))
Call("named-long", Load("named-long", "error('boom')", string.rep("n", 120)))
Call("index", Load("index", "local Missing = nil\nreturn Missing.Field", "=Name"))
Call("traceback", Load("traceback", "return (string.gsub(debug.traceback('t'), '\\n.*', ''))", "=Name"))
Call("inner-info", Load("inner-info", "local function Inner() return debug.info(1, 's'), debug.info(1, 'l'), debug.info(2, 'l') end return Inner()", "=Name"))
Call("empty", Load("empty", ""))
Call("comment", Load("comment", "--!strict\n--!native\nreturn 1"))
Load("syntax", "local = 1")
Load("syntax-named", "local = 1", "=Name")
Load("syntax-plain-name", "return +", "Name")
Load("syntax-lines", "local A = 1\nlocal B = (\nreturn")
Load("bytecode", "\27Lua")
Load("bytecode-zero", "\0\1\2")
Load("no-argument")
Load("nil", nil)
Call("number", Load("number", 12))
Load("table", {})
Load("function", function()
	return "return 1"
end)
Load("boolean", true)

-- The environment: the calling script's, read when the chunk runs
ParityRan = nil
local Setter = Load("not-run", "ParityRan = (ParityRan or 0) + 1 return ParityRan")
print("not-run before " .. tostring(ParityRan))
Call("not-run", Setter)
Call("not-run again", Setter)
print("not-run after " .. tostring(ParityRan) .. " raw " .. tostring(rawget(getfenv(1), "ParityRan")))
local Reader = Load("env", "return getfenv(1), script, getfenv(0), _G, shared, require")
if Reader then
	local Env, Script, Thread, Globals, Shared, Require = Reader()
	print("env same-as-caller " .. tostring(rawequal(Env, getfenv(1))) .. " script " .. tostring(rawequal(Script, script))
		.. " thread-env " .. tostring(rawequal(Thread, getfenv(0))) .. " thread-is-env " .. tostring(rawequal(Thread, Env))
		.. " _G " .. tostring(rawequal(Globals, _G)) .. " shared " .. tostring(rawequal(Shared, shared)) .. " require " .. tostring(rawequal(Require, require)))
	print("env getfenv(f) " .. tostring(rawequal(getfenv(Reader), getfenv(1))))
end
local Again = Load("again", "return 1")
local Twin = Load("again", "return 1")
print("again same-function " .. tostring(Again ~= nil and rawequal(Again, Twin)))
local Fenced = Load("setfenv", "return Marker, type(print), script")
if Fenced then
	print("setfenv " .. P.try(function()
		return setfenv(Fenced, { Marker = "replaced", type = type }) == Fenced
	end))
	Call("setfenv", Fenced)
	print("setfenv caller-untouched " .. tostring(Marker) .. " " .. type(print))
	local Layered = setmetatable({ Marker = "layered" }, { __index = getfenv(1) })
	setfenv(Fenced, Layered)
	print("setfenv layered " .. P.try(function()
		local Value, Kind, Script = Fenced()
		return Value, Kind, rawequal(Script, script)
	end))
end
local Local = "upvalue"
Call("no-upvalues", Load("no-upvalues", "return Local"))
Call("yields", Load("yields", "task.wait() return 'waited'"))
