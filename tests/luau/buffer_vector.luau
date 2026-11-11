-- buffer.readvectorf32x2/x3 and buffer.writevectorf32x2/x3 (patches/luau/18-buffer-vector.patch): the builtin a direct
-- call compiles to, the library function a call through a value reaches, and the float32 calls their design defines
-- them by all agree, on every offset around the end of small buffers and on the values a float32 can hold.
local function Check(Name, Actual, Expected)
	assert(Actual == Expected, Name .. ": " .. tostring(Actual) .. " ~= " .. tostring(Expected))
end

local function Bytes(Target)
	local Out = {}
	for Index = 0, buffer.len(Target) - 1 do
		table.insert(Out, string.format("%02X", buffer.readu8(Target, Index)))
	end
	return table.concat(Out)
end

-- a vector's components as the bits of their float32s (NaN and -0 compare as themselves)
local Scratch = buffer.create(12)
local function Bits(Value)
	buffer.writef32(Scratch, 0, Value.x)
	buffer.writef32(Scratch, 4, Value.y)
	buffer.writef32(Scratch, 8, Value.z)
	return Bytes(Scratch)
end

-- the library functions, reached as values (no builtin call)
local Library = {}
for _, Name in { "readvectorf32x2", "readvectorf32x3", "writevectorf32x2", "writevectorf32x3" } do
	Library[Name] = (buffer :: any)[Name]
	Check("the function " .. Name, type(Library[Name]), "function")
end

-- the builtins
local Builtin = {
	readvectorf32x2 = function(Target, Offset)
		return buffer.readvectorf32x2(Target, Offset)
	end,
	readvectorf32x3 = function(Target, Offset)
		return buffer.readvectorf32x3(Target, Offset)
	end,
	writevectorf32x2 = function(Target, Offset, Value)
		buffer.writevectorf32x2(Target, Offset, Value)
	end,
	writevectorf32x3 = function(Target, Offset, Value)
		buffer.writevectorf32x3(Target, Offset, Value)
	end,
}

-- the design's equivalents
local Reference = {
	readvectorf32x2 = function(Target, Offset)
		return vector.create(buffer.readf32(Target, Offset), buffer.readf32(Target, Offset + 4), 0)
	end,
	readvectorf32x3 = function(Target, Offset)
		return vector.create(buffer.readf32(Target, Offset), buffer.readf32(Target, Offset + 4), buffer.readf32(Target, Offset + 8))
	end,
	writevectorf32x2 = function(Target, Offset, Value)
		buffer.readf32(Target, Offset + 4)
		buffer.writef32(Target, Offset, Value.x)
		buffer.writef32(Target, Offset + 4, Value.y)
	end,
	writevectorf32x3 = function(Target, Offset, Value)
		buffer.readf32(Target, Offset + 8)
		buffer.writef32(Target, Offset, Value.x)
		buffer.writef32(Target, Offset + 4, Value.y)
		buffer.writef32(Target, Offset + 8, Value.z)
	end,
}

local function Outcome(Function, ...)
	local Ok, Result = pcall(Function, ...)
	if not Ok then
		-- (without the position a Luau caller's line adds)
		return "error " .. (string.gsub(tostring(Result), "^%[string [^%]]*%]:%d+: ", ""))
	end
	return if Result == nil then "ok" else "ok " .. Bits(Result)
end

-- every size and offset: the three agree on the outcome, the message and the bytes left behind
local Values = { vector.create(1.5, -2.25, 1 / 3), vector.create(math.huge, -math.huge, 0 / 0), vector.create(-0, 1e-45, 3.4028234663852886e38), vector.create(0.1, -123456.789, 16777217) }
local Cases = 0
for Size = 0, 20 do
	for Offset = -3, Size + 3 do
		for Index, Value in Values do
			for _, Width in { "x2", "x3" } do
				local Expected, Seen
				for Kind, Implementation in { Reference = Reference, Library = Library, Builtin = Builtin } :: { [string]: any } do
					local Target = buffer.create(Size)
					buffer.fill(Target, 0, 0xAA)
					local Wrote = Outcome(Implementation["writevectorf32" .. Width], Target, Offset, Value)
					local Read = Outcome(Implementation["readvectorf32" .. Width], Target, Offset)
					local Line = Wrote .. " / " .. Read .. " / " .. Bytes(Target)
					if Expected == nil then
						Expected, Seen = Line, Kind
					else
						Check(`{Kind} against {Seen}, {Width} size {Size} offset {Offset} value {Index}`, Line, Expected)
					end
					Cases += 1
				end
			end
		end
	end
end
-- (sizes 0 to 20 with size + 7 offsets each, four values, two widths, three implementations)
Check("cases", Cases, 357 * 4 * 2 * 3)

-- exact bytes and values
local Target = buffer.create(16)
buffer.fill(Target, 0, 0xAA)
buffer.writevectorf32x3(Target, 4, vector.create(1, 2, 3))
Check("x3 bytes", Bytes(Target), "AAAAAAAA0000803F0000004000004040")
buffer.writevectorf32x2(Target, 0, vector.create(1.5, -2.25, 99))
Check("x2 leaves z unwritten", Bytes(Target), "0000C03F000010C00000004000004040")
Check("x3 read", Bits(buffer.readvectorf32x3(Target, 4)), Bits(vector.create(-2.25, 2, 3)))
Check("x2 read zeroes z", Bits(buffer.readvectorf32x2(Target, 8)), Bits(vector.create(2, 3, 0)))
Check("unaligned", Bits(Library.readvectorf32x3(Target, 1)), Bits(Reference.readvectorf32x3(Target, 1)))
Check("a read's type", type(buffer.readvectorf32x2(Target, 0)), "vector")
Check("one result", select("#", buffer.readvectorf32x3(Target, 0)), 1)
Check("a write's results", select("#", buffer.writevectorf32x3(Target, 0, vector.zero)), 0)
Check("a library write's results", select("#", Library.writevectorf32x2(Target, 0, vector.zero)), 0)

-- a NaN's payload and sign are kept (the bytes move; no float32 is rebuilt from a double)
buffer.writeu32(Target, 0, 0xFFC12345)
buffer.writeu32(Target, 4, 0x7FC00001)
buffer.writeu32(Target, 8, 0x80000000)
for Kind, Implementation in { Library = Library, Builtin = Builtin } :: { [string]: any } do
	local Copy = buffer.create(12)
	Implementation.writevectorf32x3(Copy, 0, Implementation.readvectorf32x3(Target, 0))
	Check(Kind .. " NaN payload", Bytes(Copy), "4523C1FF0100C07F00000080")
end

-- offsets as buffer.readf32 takes them: truncated toward zero, a numeric string converted, anything else refused
for Kind, Implementation in { Library = Library, Builtin = Builtin } :: { [string]: any } do
	buffer.writevectorf32x3(Target, 0, vector.create(1, 2, 3))
	buffer.writef32(Target, 12, 4)
	Check(Kind .. " fraction", Outcome(Implementation.readvectorf32x3, Target, 4.9), "ok " .. Bits(vector.create(2, 3, 4)))
	Check(Kind .. " negative fraction", Outcome(Implementation.readvectorf32x3, Target, -0.5), "ok " .. Bits(vector.create(1, 2, 3)))
	Check(Kind .. " string offset", Outcome(Implementation.readvectorf32x2, Target, "8"), "ok " .. Bits(vector.create(3, 4, 0)))
	for _, Offset in { 0 / 0, math.huge, -math.huge, 2 ^ 31, 2 ^ 32, 2 ^ 32 + 4, -2 ^ 31, 2 ^ 31 - 4, 1e300, 5, 16, -1 } do
		Check(`{Kind} read at {Offset}`, Outcome(Implementation.readvectorf32x3, Target, Offset), "error buffer access out of bounds")
		Check(`{Kind} write at {Offset}`, Outcome(Implementation.writevectorf32x3, Target, Offset, vector.one), "error buffer access out of bounds")
	end
	Check(Kind .. " x2 last", Outcome(Implementation.readvectorf32x2, Target, 8), "ok " .. Bits(vector.create(3, 4, 0)))
	Check(Kind .. " x2 past", Outcome(Implementation.readvectorf32x2, Target, 9), "error buffer access out of bounds")
	Check(Kind .. " extra arguments", Outcome(Implementation.readvectorf32x3, Target, 0, "more", {}), "ok " .. Bits(vector.create(1, 2, 3)))
end

-- argument errors: the first bad argument is blamed, a bad value before the bounds, and nothing is written
buffer.fill(Target, 0, 0x55)
local function Refused(Name, Expected, ...)
	local Arguments = table.pack(...)
	Check("library " .. Name, Outcome(Library[Arguments[1]], table.unpack(Arguments, 2, Arguments.n)), "error " .. Expected)
end
Refused("no arguments", "missing argument #1 to 'readvectorf32x3' (buffer expected)", "readvectorf32x3")
Refused("no offset", "missing argument #2 to 'readvectorf32x2' (number expected)", "readvectorf32x2", Target)
Refused("a string for the buffer", "invalid argument #1 to 'readvectorf32x3' (buffer expected, got string)", "readvectorf32x3", "0123456789ab", 0)
Refused("a table offset", "invalid argument #2 to 'readvectorf32x3' (number expected, got table)", "readvectorf32x3", Target, {})
Refused("a text offset", "invalid argument #2 to 'readvectorf32x2' (number expected, got string)", "readvectorf32x2", Target, "four")
Refused("no value", "missing argument #3 to 'writevectorf32x3' (vector expected)", "writevectorf32x3", Target, 0)
Refused("a number value", "invalid argument #3 to 'writevectorf32x3' (vector expected, got number)", "writevectorf32x3", Target, 0, 1)
Refused("three numbers", "invalid argument #3 to 'writevectorf32x2' (vector expected, got number)", "writevectorf32x2", Target, 0, 1, 2, 3)
Refused("a table value", "invalid argument #3 to 'writevectorf32x2' (vector expected, got table)", "writevectorf32x2", Target, 0, { 1, 2, 3 })
Refused("a bad value out of bounds", "invalid argument #3 to 'writevectorf32x3' (vector expected, got number)", "writevectorf32x3", Target, 99, 1)
Refused("a bad offset and value", "invalid argument #2 to 'writevectorf32x3' (number expected, got table)", "writevectorf32x3", Target, {}, 1)
Refused("a bad buffer and offset", "invalid argument #1 to 'writevectorf32x3' (buffer expected, got number)", "writevectorf32x3", 1, {}, 1)
-- (a builtin that declines falls back to the library function, so a direct call raises the same)
Check("builtin a number value", Outcome(function()
	buffer.writevectorf32x3(Target, 0, 1)
end), "error invalid argument #3 to 'writevectorf32x3' (vector expected, got number)")
Check("builtin no offset", Outcome(function()
	return buffer.readvectorf32x2(Target)
end), "error missing argument #2 to 'readvectorf32x2' (number expected)")
Check("nothing written by a refused call", Bytes(Target), string.rep("55", 16))

return Cases
