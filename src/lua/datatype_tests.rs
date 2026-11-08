//! Ports of runtime/tests/random.luau and the checks of runtime/tests/smoke.luau that
//! engine::tests does not already make (tests.rs covers smoke's bad assignment / member errors and
//! the deferred Changed order).
//!
//! Not ported: random.luau's cloneProxy block (a Lune native-host builtin, not an engine feature)
//! and Types.Is (the engine's typeof is the check); smoke.luau's project mount and vide require
//! (they need an external game checkout; the story / session tests mount projects).

use std::rc::Rc;

use mlua::prelude::*;

use crate::{
    Engine, Options,
    lua::{self as engine_lua, globals::CallOutcome},
};

fn assert_script(source: &str) -> Vec<String> {
    let engine: Rc<Engine> = Engine::new(Options::default()).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        crate::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
    let function = lua.load(source).set_name("=Test").set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
    if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
        panic!("test script failed: {message}");
    }
    let lines: Vec<String> = engine.state.borrow().output.entries.iter().map(|e| format!("{}: {}", e.kind, e.message)).collect();
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("error")).collect();
    assert!(errors.is_empty(), "{errors:?}");
    lines
}

/// runtime/tests/random.luau: golden PCG32 vectors and streams, range boundaries, per-object
/// state, Clone, seed edges, the deterministic no-seed sequence and the locked userdata.
#[test]
fn random_streams_ranges_identity_and_seeds() {
    assert_script(
        r#"
-- Independent integer PCG32 oracle: uint64 state = state*6364136223846793005+105,
-- two uint32 draws per double (low first), multiply-high range reduction for integers under 2^32;
-- a wider range is floor(min * (1 - u) + max * u), as measured in a playing client (Studio 0.744,
-- tools/parity/probes/random.luau; the oracle's 64-bit multiply gave one more for seeds 7, -1.9, -2^53 + 1).
local Vectors = {
	{ 0, { 0.5514556904749074, 0.01723102436113014, 0.883376920906403, 73, 72384586732 } },
	{ 7, { 0.8737085924861333, 0.4321572161309693, 0.3089922198934871, 5, 482964445446 } },
	{ -3, { 0.23554798859579634, 0.32381286642224644, 0.14705557445875167, 28, -493285887950 } },
	{ 1.9, { 0.9484344206192203, 0.061285879733742145, 0.8770159548057006, 9, -497511649161 } },
	{ -1.9, { 0.0037155688242635435, 0.5469862230236278, 0.31011551315193603, 67, 1062967531246 } },
	{ 4294967303, { 0.9971430285879401, 0.5584106571948971, 0.05136057646030647, 58, -943718980273 } },
	{ 9007199254740991, { 0.027684032073451197, 0.09103798432822455, 0.9869885996197573, 53, 502533245394 } },
	{ -9007199254740991, { 0.22788703655698744, 0.9425859688069043, 0.06775640206431996, 38, -518359285893 } },
}
local function CheckVectors()
	for _, Case in Vectors do
		local R = Random.new(Case[1])
		for Index = 1, 3 do
			assert(R:NextNumber() == Case[2][Index], `seed {Case[1]} double {Index}`)
		end
		assert(R:NextInteger(1, 100) == Case[2][4], `seed {Case[1]} integer`)
		assert(R:NextInteger(-1099511627776, 1099511628776) == Case[2][5], `seed {Case[1]} wide integer`)
	end
end
CheckVectors()
CheckVectors() -- warm seeds must return the original initial states
local A, B = Random.new(7), Random.new(7)
assert(A ~= B and type(A) == "userdata" and typeof(A) == "Random")
assert(A:NextNumber() == Vectors[2][2][1])
assert(A:NextNumber() == Vectors[2][2][2])
assert(B:NextNumber() == Vectors[2][2][1], "same-seed objects shared their mutable state")
local Clone = A:Clone()
assert(Clone ~= A and typeof(Clone) == "Random")
for _ = 1, 100 do
	assert(A:NextInteger(-1000000, 1000000) == Clone:NextInteger(-1000000, 1000000))
end
local Held = Random.new(7)
Held:NextNumber()
for Seed = 10000, 20000 do
	Random.new(Seed):NextNumber()
end
assert(Held:NextNumber() == Vectors[2][2][2], "cache eviction mutated a live stream")
CheckVectors()
assert(Random.new(-0.0):NextNumber() == Random.new(0):NextNumber())
for _, Seed in { 4294967295, 4294967296, 4294967297, 1e20, -1e20 } do
	local First, Second = Random.new(Seed), Random.new(Seed)
	for _ = 1, 20 do
		assert(First:NextNumber() == Second:NextNumber())
	end
end
-- Preserve the pre-cache non-finite behavior and deterministic no-seed sequence.
assert(Random.new(math.huge):NextNumber() == 0)
assert(Random.new(-math.huge):NextNumber() == 0.0037155688242635435)
assert(Random.new(0 / 0):NextNumber() == 0.0037155688242635435)
for _, Expected in { 0.15903319831126075, 0.6648808275887605, 0.7537480905786614 } do
	assert(Random.new():NextNumber() == Expected, "no-seed sequence")
end
assert(not pcall(function()
	Random.new("bad" :: any)
end))
local Ok, Error = pcall(function()
	(A :: any).Anything = 1
end)
assert(not Ok and string.find(tostring(Error), "Anything cannot be assigned to", 1, true), tostring(Error))
assert(getmetatable(A) == "The metatable is locked")
assert(getmetatable(B) == getmetatable(A) and A.NextNumber == B.NextNumber)
local UnknownOk, UnknownError = pcall(function() return A.UnknownMember end)
assert(not UnknownOk and string.find(tostring(UnknownError), "UnknownMember is not a valid member of Random", 1, true))

-- Frozen independent BigInt PCG32 oracle, 256 full-width range draws per seed.
local StreamChecks = {
	{ 0, 0xdd94bea1 },
	{ 1, 0x6d0cb4df },
	{ -1, 0x50a5f481 },
	{ 65535, 0x98ec0c6c },
	{ 65536, 0x2184b84e },
	{ 65537, 0xbda7dee6 },
	{ 4294967295, 0x9f8fb1a6 },
	{ 4294967296, 0x73eaab83 },
	{ 4294967297, 0xc4d03ce3 },
	{ -4294967295, 0xe4cd4139 },
	{ -4294967296, 0x7d8e01c1 },
	{ -4294967297, 0xeecb8787 },
	{ 9007199254740991, 0x1619fc41 },
	{ -9007199254740991, 0x510353d4 },
	{ 8279140260984440, 0xf54e9519 },
	{ -3446241584225153, 0x17ed74f5 },
	{ 7966395542197526, 0xf0e372bc },
	{ -5430052476903239, 0x0cc85c5d },
	{ 2935106855172324, 0xdf70efeb },
	{ -6384148386864253, 0x92f7d8ae },
	{ 7162472545633634, 0x83e02791 },
	{ -4177724348780707, 0xd97a777c },
	{ 5977007115012624, 0xd8f5a253 },
	{ -7192467481081145, 0x806f1fde },
	{ 8863833131590254, 0x022e02d1 },
	{ -1769047220330431, 0x2d287eb7 },
	{ 3184271431751164, 0x498692f5 },
	{ -77273909666229, 0x0d23129d },
	{ 7432852227569722, 0x0ed87406 },
	{ -7968050973091995, 0xb0396b9a },
	{ 5541507801188520, 0xbfbb9072 },
	{ -188557386145265, 0x8839f308 },
	{ 7918227809178310, 0xbaec7144 },
	{ -556657445603127, 0x68f0b27e },
	{ 131636434112020, 0x7ba7b392 },
	{ -5830691134858733, 0x86112b29 },
	{ 617780101152274, 0xcc48cde6 },
	{ -6276599599929235, 0x4a3f2caf },
	{ 6028669383217728, 0x2de43a6c },
	{ -4391824743921065, 0x05a55762 },
	{ 2734820613752350, 0x1edd24c1 },
	{ -2778718176081327, 0x81dfb69b },
	{ 2780458655416620, 0x81552f5c },
	{ -4009636752066853, 0xf4634cd5 },
	{ 1564623516530410, 0x3858b863 },
	{ -6651977796910475, 0x20a06481 },
	{ 924297076672216, 0x8a452956 },
	{ -119958211812449, 0x9da42bd2 },
}
for _, Case in StreamChecks do
	local Generator = Random.new(Case[1])
	local Checksum = 0
	for _ = 1, 256 do
		Checksum = bit32.bxor(bit32.lrotate(Checksum, 5), Generator:NextInteger(0, 4294967294))
	end
	assert(Checksum == Case[2], `PCG32 stream mismatch for seed {Case[1]}`)
end
-- The exact-product boundary, signed/fractional bounds, and reversed bounds.
local RangeChecks = {
	{ 0, 0, 0x00000000 },
	{ 1, 12, 0xe9473863 },
	{ -1.9, 1.9, 0x4aef8dba },
	{ 10.9, -4.2, 0x0f06db22 },
	{ 0, 2097150, 0xd866f5e9 },
	{ 0, 2097151, 0x619d6445 },
	{ 0, 2097152, 0xc3fcced7 },
	{ -1048576, 1048575, 0xa06b9405 },
	{ -1048576, 1048576, 0x020a3e97 },
	{ 4294967296, 4294967306, 0x27f60369 },
}
for _, Case in RangeChecks do
	local Generator = Random.new(7)
	local Checksum = 0
	for _ = 1, 256 do
		Checksum = bit32.bxor(bit32.lrotate(Checksum, 5), Generator:NextInteger(Case[1], Case[2]))
	end
	assert(Checksum == Case[3], `PCG32 range mismatch for {Case[1]}, {Case[2]}`)
end
"#,
    );
}

/// runtime/tests/smoke.luau's instance checks (its printed values as assertions): Frame
/// defaults, assignment, the two-argument Instance.new, number-to-string Text, full names.
#[test]
fn smoke_frame_defaults_and_parenting() {
    assert_script(
        r#"
local Frame = Instance.new("Frame")
assert(Frame.Size == UDim2.new(0, 0, 0, 0))
assert(Frame.BackgroundColor3 == Color3.fromRGB(163, 162, 165))
assert(Frame.BorderSizePixel == 1 and Frame.ClassName == "Frame" and Frame.Name == "Frame")
Frame.Size = UDim2.new(0, 100, 0, 50)
assert(Frame.Size == UDim2.new(0, 100, 0, 50) and typeof(Frame.Size) == "UDim2" and typeof(Frame) == "Instance")
local Label = Instance.new("TextLabel", Frame)
Label.Text = 5
assert(Label.Text == "5" and Label.Parent == Frame)
assert(Label:GetFullName() == "Frame.TextLabel" and Frame:FindFirstChild("TextLabel") == Label)
"#,
    );
}

/// Constructor rules the engine enforces as Roblox does: sequence keypoints (Studio's messages where
/// the developer forum records them), the envelope, NumberRange, BrickColor's zero-based palette and
/// default, PhysicalProperties' clamps, 16-bit overflow, Content, Font.fromEnum and DateTime ranges.
#[test]
fn datatype_constructors_follow_roblox_rules() {
    assert_script(
        r#"
local function Fails(Expected, Function, ...)
	local Ok, Message = pcall(Function, ...)
	assert(not Ok, `no error, expected "{Expected}"`)
	assert(tostring(Message) == Expected, `got "{Message}", expected "{Expected}"`)
end
local function Near(A, B)
	return math.abs(A - B) < 1e-6
end

local function NK(Time, Value, Envelope)
	return NumberSequenceKeypoint.new(Time, Value, Envelope)
end
local function Evenly(Count, Make)
	local Keypoints = {}
	for Index = 0, Count - 1 do
		table.insert(Keypoints, Make(Index / (Count - 1)))
	end
	return Keypoints
end
assert(#NumberSequence.new(Evenly(20, function(Time) return NK(Time, 0) end)).Keypoints == 20)
Fails("NumberSequence.new(): table is too long.", NumberSequence.new, Evenly(21, function(Time) return NK(Time, 0) end))
Fails("NumberSequence: requires at least 2 keypoints", NumberSequence.new, { NK(0, 1) })
Fails("NumberSequence: requires at least 2 keypoints", NumberSequence.new, {})
Fails("NumberSequence: all keypoints must be ordered by time", NumberSequence.new, { NK(0, 1), NK(0.6, 1), NK(0.5, 1), NK(1, 1) })
Fails("NumberSequence must start at time=0.0", NumberSequence.new, { NK(0.1, 1), NK(1, 1) })
Fails("NumberSequence must end at time=1.0", NumberSequence.new, { NK(0, 1), NK(0.9, 1) })
Fails("NumberSequence.new(): table of NumberSequenceKeypoints expected.", NumberSequence.new, NK(0, 1))
Fails("NumberSequence.new(): table of NumberSequenceKeypoints expected.", NumberSequence.new, { 0, 1 })
assert(#NumberSequence.new({ NK(0, 1), NK(0.5, 2), NK(0.5, 3), NK(1, 4) }).Keypoints == 4, "equal times are in order")
local Snapped = NumberSequence.new({ NK(0.00005, 1), NK(0.99995, 2) }).Keypoints
assert(Snapped[1].Time == 0 and Snapped[2].Time == 1, "ends within the tolerance snap to 0 and 1")
assert(NumberSequence.new(1, 2).Keypoints[2].Value == 2 and #NumberSequence.new(3).Keypoints == 2)

local Red = Color3.new(1, 0, 0)
local function CK(Time)
	return ColorSequenceKeypoint.new(Time, Red)
end
assert(#ColorSequence.new(Evenly(20, CK)).Keypoints == 20)
Fails("ColorSequence.new(): table is too long.", ColorSequence.new, Evenly(21, CK))
Fails("ColorSequence: requires at least 2 keypoints", ColorSequence.new, { CK(0) })
Fails("ColorSequence: all keypoints must be ordered by time", ColorSequence.new, { CK(0), CK(0.6), CK(0.5), CK(1) })
-- The developer forum's case: times 0..7 are in order and end at 7
Fails("ColorSequence must end at time=1.0", ColorSequence.new, { CK(0), CK(1), CK(2), CK(7) })
Fails("ColorSequence must start at time=0.0", ColorSequence.new, { CK(0.5), CK(1) })
Fails("ColorSequence.new(): table expected", ColorSequence.new)
Fails("ColorSequence.new(): table of ColorSequenceKeypoints expected.", ColorSequence.new, { CK(0), 1 })
assert(ColorSequence.new(Red, Color3.new(0, 0, 1)).Keypoints[2].Value == Color3.new(0, 0, 1))

Fails("NumberSequenceKeypoint: envelope must be non-negative", NumberSequenceKeypoint.new, 0, 1, -0.5)
assert(NK(0.5, 1, 0).Envelope == 0 and NK(0.5, 1).Envelope == 0 and Near(NK(0.5, 1, 0.25).Envelope, 0.25))
assert(NK(2, 1).Time == 2, "a keypoint's time is checked by the sequence, not the keypoint")

Fails("NumberRange: invalid range", NumberRange.new, 2, 1)
local Range = NumberRange.new(1, 2)
assert(Range.Min == 1 and Range.Max == 2 and NumberRange.new(3).Min == 3 and NumberRange.new(1, 1).Max == 1)

assert(BrickColor.palette(0).Name == "Earth green" and BrickColor.palette(127).Number == 1003, "palette indices are zero-based")
Fails("Invalid index", BrickColor.palette, 128)
Fails("Invalid index", BrickColor.palette, -1)
assert(BrickColor.new("Not a colour").Name == "Medium stone grey" and BrickColor.new(9999).Name == "Medium stone grey")
assert(BrickColor.new("Bright red").Number == 21)
-- three numbers are 0-1 components (the nearest by summed channel differences), as is a Color3
assert(BrickColor.new(1, 0, 0).Name == "Really red" and BrickColor.new(0, 1, 0).Name == "Lime green")
assert(BrickColor.new(0.502, 0.734, 0.859).Name == "Pastel Blue", "Roblox's documented example")
assert(BrickColor.new(Color3.new(1, 0, 0)).Name == "Really red" and BrickColor.new(Color3.fromRGB(128, 187, 219)).Name == "Pastel Blue")
assert(BrickColor.new(0, 0, 0).Name == "Really black" and BrickColor.new(21).Name == "Bright red")

local Clamped = PhysicalProperties.new(-1, 5, 2, 200, -3)
assert(Near(Clamped.Density, 0.0001) and Clamped.Friction == 2 and Clamped.Elasticity == 1, "clamped")
assert(Clamped.FrictionWeight == 100 and Clamped.ElasticityWeight == 0, "weights clamped")
local Plain = PhysicalProperties.new(0.7, 0.3, 0.5)
assert(Near(Plain.Density, 0.7) and Near(Plain.Friction, 0.3) and Plain.FrictionWeight == 1)

local Wrapped = Vector3int16.new(32768, 40000, -32769)
assert(Wrapped.X == -32768 and Wrapped.Y == -25536 and Wrapped.Z == 32767, "components overflow 16 bits")
-- (truncated toward zero, as a playing client measures: Studio 0.744; the documentation says rounded down)
local Truncated = Vector2int16.new(1.9, -1.5)
assert(Truncated.X == 1 and Truncated.Y == -1, "components are truncated toward zero")
assert(Vector3int16.new(1, 2, 3) + Vector3int16.new(32767, 0, 0) == Vector3int16.new(-32768, 2, 3), "sums overflow 16 bits")
assert(Vector3int16.new().X == 0 and tostring(Vector3int16.new(5, 6, 7)) == "5, 6, 7")

-- a Vector3 component by a key known only at run time (the compiler folds constant keys)
local Axes = { "X", "y", "Z" }
local Point = Vector3.new(4, 5, 6)
for Index, Axis in Axes do
	assert(Point[Axis] == ({ 4, 5, 6 })[Index], Axis)
end
local Ok, Message = pcall(function() return Point[Axes[1] .. "Nope"] end)
assert(not Ok and string.find(tostring(Message), "XNope is not a valid member of Vector3", 1, true), tostring(Message))

assert(Content.fromAssetId(0) == Content.none and Content.fromAssetId(12).Uri == "rbxassetid://12")
Fails("invalid argument #1 to 'fromAssetId' (finite number expected)", Content.fromAssetId, math.huge)
Fails("invalid argument #1 to 'fromAssetId' (finite number expected)", Content.fromAssetId, 0 / 0)
Fails("invalid argument #1 to 'fromObject' (Object expected, got nil)", Content.fromObject, nil)
assert(Content.fromObject(Instance.new("Folder")).SourceType == Enum.ContentSourceType.Object)

Fails("invalid argument #1 to 'fromEnum' (Enum.Font.Unknown has no font face)", Font.fromEnum, Enum.Font.Unknown)
assert(Font.fromEnum(Enum.Font.Gotham).Family == "rbxasset://fonts/families/GothamSSm.json")

assert(DateTime.fromUnixTimestamp(253402300799):ToIsoDate() == "9999-12-31T23:59:59Z")
assert(DateTime.fromUnixTimestamp(-17987443200):ToIsoDate() == "1400-01-01T00:00:00Z")
-- the messages a playing client raises (Studio 0.744, tools/parity/probes/datetime.luau; fromUnixTimestampMillis's is
-- not measured)
local OutOfRange = "Error interpreting timestamp as a precise point in time. Fields out of range. UnixTimestamp should be between -17987443200 and 253402300799."
local NoDate = "Error interpreting year, month, day, hour, minute, second as a precise point in time. Fields out of range."
Fails(OutOfRange, DateTime.fromUnixTimestamp, 253402300800)
Fails(OutOfRange, DateTime.fromUnixTimestamp, -17987443201)
Fails(OutOfRange, DateTime.fromUnixTimestamp, 0 / 0)
Fails(OutOfRange, DateTime.fromUnixTimestampMillis, 253402300800000)
assert(DateTime.fromUnixTimestampMillis(253402300799999).UnixTimestampMillis == 253402300799999)
Fails(NoDate, DateTime.fromUniversalTime, 2023, 2, 29)
Fails(NoDate, DateTime.fromUniversalTime, 2023, 1, 32)
Fails(NoDate, DateTime.fromLocalTime, 2023, 13, 1)
Fails(NoDate, DateTime.fromUniversalTime, -1, 1, 1)
Fails("Over the max DateTime supported: 9999-12-31T23:59:59Z", DateTime.fromUniversalTime, 10000, 1, 1)
assert(DateTime.fromUniversalTime(2021.5, 1.5, 1.5).UnixTimestampMillis == 1609459200000, "units are truncated")
-- timestamps are truncated toward zero; UnixTimestamp too, while the calendar fields count down from the instant
assert(DateTime.fromUnixTimestamp(-1.5).UnixTimestampMillis == -1000 and DateTime.fromUnixTimestamp(1.999).UnixTimestampMillis == 1000)
local Before = DateTime.fromUnixTimestampMillis(-1.5)
assert(Before.UnixTimestampMillis == -1 and Before.UnixTimestamp == 0 and Before:ToUniversalTime().Second == 59)
assert(tostring(DateTime.fromUnixTimestampMillis(-0.5)) == "0" and tostring(DateTime.fromUnixTimestamp(-0.5)) == "0")
-- ISO 8601 as a playing client reads it
assert(DateTime.fromIsoDate("2020-02-29T12:34:56.789+02:00").UnixTimestampMillis == 1582972496789)
assert(DateTime.fromIsoDate(" 2020-02-29 12:34:56.1234z").UnixTimestampMillis == 1582979696123)
assert(DateTime.fromIsoDate("2020-02-29").UnixTimestampMillis == 1582934400000 and DateTime.fromIsoDate("2020-02-29T12:34").UnixTimestampMillis == 1582979640000)
assert(DateTime.fromIsoDate("2020-02-29T24:00:00Z").UnixTimestampMillis == 1583020800000, "hour 24 is the next day")
assert(DateTime.fromIsoDate("2020-02-29T12:34:56+0200") == nil and DateTime.fromIsoDate("20200229T123456Z") == nil)
-- formats: no ordinals, `SSSS` is milliseconds then tenths, the locale must be a string
local Instant = DateTime.fromUnixTimestampMillis(1700000000123)
assert(Instant:FormatUniversalTime("Do SSSS d [YYYY] LLLL", "en-us") == "14o 1231 2 YYYY Tuesday, November 14, 2023 10:13 PM")
Fails("invalid argument #3 to 'FormatUniversalTime' (string expected, got nil)", Instant.FormatUniversalTime, Instant, "LL")
-- os.date and os.time are Luau's, with its plain messages
Fails("invalid argument #1 to 'date' (invalid conversion specifier)", os.date, "!%D", 0)
Fails("invalid argument #1 to 'time' (table expected, got number)", os.time, 5)
assert(os.date("!%c", 1700000000) == "Tue Nov 14 22:13:20 2023" and os.date("!%Y", -1) == "1969")
Fails("invalid argument #1 to 'fromUniversalTime' (number expected, got table)", DateTime.fromUniversalTime, {})
assert(DateTime.fromUniversalTime(2024, 2, 29):ToIsoDate() == "2024-02-29T00:00:00Z")
assert(DateTime.fromUniversalTime(2023, 1, 1, 0, 90, -10):ToIsoDate() == "2023-01-01T01:29:50Z", "time units roll over")
assert(DateTime.fromUniversalTime().UnixTimestamp == 0 and DateTime.fromUniversalTime("2000").UnixTimestamp == 946684800)
assert(DateTime.fromIsoDate("2023-02-30T00:00:00Z") == nil and DateTime.fromIsoDate("2023-02-28T00:00:00Z") ~= nil)
"#,
    );
}

/// Vector3 Magnitude and Unit are answered by the VM (patches/luau/05-vector-members.patch, luaV_vectormember) and are what the single
/// precision sums of squares give: Magnitude is `sqrtf(x*x + y*y + z*z)` widened to a double and Unit scales each component
/// by `1.0f / sqrtf(...)`, bit for bit, as the `vector` library computes them.
#[test]
fn vector3_magnitude_and_unit_are_single_precision_to_the_bit() {
    let lines = assert_script(
        r#"
        local Generator = Random.new(42)
        local Cases = {}
        for Index = 1, 600 do
            local Scale = 10 ^ Generator:NextInteger(-3, 4)
            Cases[Index] = Vector3.new(Generator:NextNumber(-1, 1) * Scale, Generator:NextNumber(-1, 1) * Scale, Generator:NextNumber(-1, 1) * Scale)
        end
        Cases[#Cases + 1] = Vector3.new(0, 0, 0)
        Cases[#Cases + 1] = Vector3.new(3, 4, 0)
        Cases[#Cases + 1] = Vector3.new(1e20, 1e20, 1e20)
        for _, V in Cases do
            assert(V.Magnitude == vector.magnitude(V) and V.magnitude == V.Magnitude, "Magnitude is vector.magnitude")
            local U = V.Unit
            local Want = vector.normalize(V)
            assert(U == Want or (U ~= U and Want ~= Want), "Unit is vector.normalize")
            assert(V.unit == U or U ~= U)
            -- the zero vector's Unit is NaN, which prints differently on each host
            if V.Magnitude > 0 then
                print(string.format("%.17g %.17g %.17g %.17g %.17g %.17g %.17g", V.X, V.Y, V.Z, V.Magnitude, U.X, U.Y, U.Z))
            end
        end
        "#,
    );
    let rows: Vec<Vec<f64>> = lines.iter().filter_map(|line| line.strip_prefix("print: ")).map(|row| row.split(' ').map(|n| n.parse().unwrap()).collect()).collect();
    assert_eq!(rows.len(), 602);
    for row in &rows {
        let (x, y, z) = (row[0] as f32, row[1] as f32, row[2] as f32);
        let length = (x * x + y * y + z * z).sqrt();
        assert_eq!(row[3].to_bits(), (length as f64).to_bits(), "Magnitude of ({x}, {y}, {z})");
        let inverse = 1.0f32 / length;
        for (axis, component) in [x, y, z].into_iter().enumerate() {
            let want = component * inverse;
            let got = row[4 + axis] as f32;
            assert!(got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()), "Unit.{axis} of ({x}, {y}, {z}): {got} against {want}");
        }
    }
}

/// A Vector3 answers its components by a key known only at run time, its methods from a table, and any other member with
/// Studio's error raised at the line that read it (level 2), whether it is read, called or indexed by a number (the
/// metatable's `__index` used to be a function of the engine's Luau; it is a table now and the VM answers the rest).
#[test]
fn vector3_members_methods_and_errors_follow_studio() {
    assert_script(
        r#"
        local V, W = Vector3.new(3, 4, 12), Vector3.new(1, 2, 3)
        local Keys = { "x", "Y", "z" }
        for Index, Key in Keys do
            assert(V[Key] == ({ 3, 4, 12 })[Index], Key)
        end
        assert(V.Magnitude == 13 and V.magnitude == 13 and V["Magnitude"] == 13 and V[("Magn" .. "itude")] == 13)
        assert(math.abs(V.Unit.Y - 4 / 13) < 1e-6)
        assert(V:Dot(W) == 3 + 8 + 36 and V.Dot == W.Dot)
        assert(V:Cross(W) == Vector3.new(-12, 3, 2))
        assert(V:Max(W) == Vector3.new(3, 4, 12) and V:Min(W) == W and V:Lerp(W, 0.5) == Vector3.new(2, 3, 7.5))
        assert(V:FuzzyEq(V + Vector3.new(1e-6, 0, 0)) and not V:FuzzyEq(W))
        assert(math.abs(V:Angle(W) - math.acos(V:Dot(W) / (V.Magnitude * W.Magnitude))) < 1e-6)
        assert(getmetatable(V) == "The metatable is locked")
        for _, Case in {
            { "Nope", function() return V.Nope end },
            { "Nope", function() return V:Nope() end },
            { "nope", function() local Key = "nope" return V[Key] end },
            { "1", function() return V[1] end },
            { "Magnitudes", function() return V.Magnitudes end },
        } do
            local Ok, Message = pcall(Case[2])
            assert(not Ok and string.find(tostring(Message), "^" .. Case[1] .. " is not a valid member of Vector3$"), tostring(Message))
        end
        "#,
    );
}

/// The lowercase names a client's datatypes answer and those it does not (a headless roblox-sim client, Studio 0.744: every member
/// of every datatype by its other first-letter case; tools/parity/probes/camelcase.luau): no rule as instances have, a list per type.
#[test]
fn datatypes_answer_the_lowercase_names_a_client_lists() {
    assert_script(
        r#"
        local CF = CFrame.new(1, 2, 3) * CFrame.Angles(0.1, 0.2, 0.3)
        assert(CF.x == CF.X and CF.y == CF.Y and CF.z == CF.Z)
        local function Same(F, G)
            local A, B = { F() }, { G() }
            return #A == 3 and table.concat(A, ",") == table.concat(B, ",")
        end
        assert(Same(function() return CF:toOrientation() end, function() return CF:ToOrientation() end))
        assert(Same(function() return CF:toEulerAnglesYXZ() end, function() return CF:ToEulerAnglesYXZ() end))
        local C3 = Color3.new(0.25, 0.5, 0.75)
        assert(C3.r == C3.R and C3.g == C3.G and C3.b == C3.B)
        assert(C3:lerp(Color3.new(1, 1, 1), 0.5) == C3:Lerp(Color3.new(1, 1, 1), 0.5))
        local V2 = Vector2.new(3, 4)
        assert(V2:lerp(Vector2.zero, 0.5) == V2:Lerp(Vector2.zero, 0.5))
        local U2 = UDim2.new(0.5, 10, 0.25, 20)
        assert(U2:lerp(UDim2.new(), 0.5) == U2:Lerp(UDim2.new(), 0.5))
        local R = Ray.new(Vector3.zero, Vector3.new(0, 0, 2))
        assert(R.unit.Direction == R.Unit.Direction and R.unit.Direction.Z == 1)
        local I3, I2 = Vector3int16.new(1, 2, 3), Vector2int16.new(4, 5)
        assert(I3.x == 1 and I3.y == 2 and I3.z == 3 and I2.x == 4 and I2.y == 5)
        local Bricks = BrickColor.new("Bright red")
        assert(Bricks.name == "Bright red" and Bricks.number == 21 and Bricks.r == Bricks.Color.R)
        assert(BrickColor.New("Bright red") == Bricks)
        -- (BrickColor answers r / g / b in lower case only)
        for _, Key in { "R", "G", "B" } do
            assert(not pcall(function() return Bricks[Key] end), Key)
        end
        local Connection = game.Changed:connect(function() end)
        assert(Connection.connected == true)
        Connection:disconnect()
        assert(Connection.connected == false and Connection.Connected == false)
        assert(typeof(game.Changed.connectParallel) == "function" and typeof(game.Changed.wait) == "function")
        -- and the names a client refuses
        assert(not pcall(function() return game.Changed:once(function() end) end))
        assert(not pcall(function() return Vector3.new(1, 2, 3):dot(Vector3.one) end))
        "#,
    );
}

/// CFrame, Vector2 and Color3 compute in a playing client's float order (types::float32): values a Studio 0.744
/// playing client printed (tools/parity/probes/datatypes.luau, cframe_order.luau; tests/parity.rs guards every
/// matching line of those probes).
#[test]
fn cframe_and_colour_math_follow_a_playing_clients_float_order() {
    assert_script(
        r#"
local function F(...)
	local Out = {}
	for _, Value in { ... } do
		table.insert(Out, string.format("%.9g", Value))
	end
	return table.concat(Out, " ")
end
local function Is(Got, Want)
	assert(Got == Want, `got "{Got}", expected "{Want}"`)
end
Is(F(CFrame.Angles(0, 0.753655374, 0):GetComponents()), "0 0 0 0.729192317 -0 0.684308827 0 1 -0 -0.684308827 0 0.729192317")
Is(tostring(CFrame.Angles(0.1, 0.2, 0.3)), "0, 0, 0, 0.936293423, -0.289629489, 0.198669329, 0.312991828, 0.944702566, -0.0978434011, -0.159345075, 0.153791994, 0.975170374")
local Order = CFrame.fromEulerAngles(0.7, -1.3, 2.1, Enum.RotationOrder.XZY)
Is(F(Order:GetComponents()), "0 0 0 -0.135045752 -0.863209426 0.486448526 -0.444133341 -0.386127561 -0.808486938 0.885724723 -0.325230747 -0.33123523")
Is(F(Order:ToEulerAngles(Enum.RotationOrder.XZY)), "-2.44159269 1.84159267 1.04159284")
-- lookAlong takes the direction itself; lookAt(at, at + direction) rounds the sum far from the origin
local Far, Direction = Vector3.new(1000.123, -2000.5, 3000.25), Vector3.new(0.3, 0.2, -0.9)
Is(F(CFrame.lookAlong(Far, Direction):GetComponents()), "1000.12299 -2000.5 3000.25 0.948683321 -0.0652328208 -0.309426397 -0 0.9784922 -0.20628427 0.316227794 0.19569844 0.928279161")
Is(F(CFrame.lookAt(Far, Far + Direction):GetComponents()), "1000.12299 -2000.5 3000.25 0.948676825 -0.0652278885 -0.30944714 -0 0.978498042 -0.206256106 0.316247076 0.195670381 0.928278446")
Is(F(CFrame.lookAt(Vector3.zero, Vector3.new(0, 10, 0)):GetComponents()), "0 0 0 0 1 -0 0 -0 -1 -1 0 -0")
-- Color3.fromRGB divides by 255, BrickColor multiplies by 1/255; pure red's hue is 1; fromHSV clamps the hue
Is(F(Color3.fromRGB(196, 40, 3).R, Color3.fromRGB(196, 40, 3).G, Color3.fromRGB(98, 37, 209).B), "0.768627465 0.156862751 0.819607854")
Is(F(BrickColor.new("Bright red").Color.R), "0.768627524")
Is(F(Color3.new(1, 0, 0):ToHSV()), "1 1 1")
Is(F(Color3.fromHSV(1, 1, 1).R, Color3.fromHSV(1.5, 1, 1).G, Color3.fromHSV(-0.25, 1, 1).B), "1 0 0")
"#,
    );
}

/// CFrame:Lerp blends the quaternions of rotations within about 2.2 degrees and slerps those farther apart, as a Studio
/// 0.746 playing client does (the components it printed; it switches between quaternion dots 0.99980998, which slerped,
/// and 0.99981022, which blended). Lerp between equal rotations was not-a-number for every alpha but 0 and 1, so a
/// tween of a part's CFrame to a goal with its own rotation turned the part not-a-number.
#[test]
fn cframe_lerp_blends_rotations_that_nearly_agree() {
    assert_script(
        r#"
local function F(...)
	local Out = {}
	for _, Value in { ... } do
		table.insert(Out, string.format("%.9g", Value))
	end
	return table.concat(Out, " ")
end
local function Is(Got, Want)
	assert(Got == Want, `got "{Got}", expected "{Want}"`)
end
local Base = CFrame.Angles(0.3, 0.7, 0.2)
Is(F(Base:GetComponents()), "0 0 0 0.749596298 -0.151950687 0.64421767 0.376380503 0.898470879 -0.226026341 -0.544465959 0.411899477 0.730681717")
-- the same rotation comes back through its quaternion, a float step off
Is(F(Base:Lerp(Base, 0.3):GetComponents()), "0 0 0 0.749596238 -0.151950702 0.64421773 0.376380503 0.898470819 -0.226026326 -0.544466019 0.411899477 0.730681658")
Is(F(Base:Lerp(Base, -0.1):GetComponents()), "0 0 0 0.749596238 -0.151950657 0.64421767 0.376380444 0.898470879 -0.226026312 -0.544465959 0.411899447 0.730681658")
-- 0.03898 radians apart blends and 0.039 slerps: far outside 0..1, the two forms differ by 4e-5
local Near, Past = Base * CFrame.Angles(0, 0.03898, 0), Base * CFrame.Angles(0, 0.039, 0)
Is(F(Near:GetComponents()), "0 0 0 0.723921657 -0.151950687 0.672940195 0.384902894 0.898470879 -0.21118705 -0.572527111 0.411899477 0.708908796")
Is(F(Base:Lerp(Near, -2):GetComponents()), "0 0 0 0.797448635 -0.151950747 0.583940625 0.357652992 0.898470819 -0.254626215 -0.485962987 0.411899447 0.770829976")
Is(F(Base:Lerp(Near, 3):GetComponents()), "0 0 0 0.669367254 -0.151950628 0.727226496 0.400167853 0.898470819 -0.180598542 -0.62594974 0.411899447 0.662212729")
Is(F(Past:GetComponents()), "0 0 0 0.723908186 -0.151950687 0.672954679 0.384907097 0.898470879 -0.211179346 -0.572541296 0.411899477 0.708897352")
Is(F(Base:Lerp(Past, -2):GetComponents()), "0 0 0 0.797515154 -0.151950657 0.583849788 0.357623935 0.898470819 -0.254666984 -0.48587516 0.411899447 0.770885348")
Is(F(Base:Lerp(Past, 3):GetComponents()), "0 0 0 0.669270039 -0.151950687 0.727316082 0.400191993 0.898470879 -0.180545002 -0.626038373 0.411899418 0.662129045")
"#,
    );
}

/// Every CFrame constructor, member, method, deprecated alias and operator gives the same bits run after run
/// (cframe_golden/ops.txt, printed with %.17g), and a misuse gives the same error text. Three misuse texts differ
/// from the build before CFrame took and returned native vectors itself: a table where a Vector3 is expected names
/// the type as 'Vector3' (it named the Rust type), and an integral number is a "number" (it was an "integer").
/// The values are a playing client's order of float operations since 2026-11-07 (types::float32; 22 lines moved in
/// their last digits from glam's quaternion and 4x4 forms: the Euler and look constructors, Inverse, Lerp,
/// Orthonormalize, the spaces and the decompositions).
#[test]
fn cframe_operations_keep_their_values_bit_for_bit() {
    let lines = assert_script(include_str!("cframe_golden/ops.luau"));
    let expected: Vec<String> = include_str!("cframe_golden/ops.txt").lines().map(|line| format!("print: {line}")).collect();
    for (index, (got, want)) in lines.iter().zip(&expected).enumerate() {
        assert_eq!(got, want, "line {}", index + 1);
    }
    assert_eq!(lines.len(), expected.len());
}

/// The work-clock profile (`RUR_LUAU_PROFILE_CLOCK=work`) weighs stacks by what the work clock charged, not by
/// the host's time: a CFrame member read costs the 15 ns host call the model fitted to Studio, plus its
/// instructions, however long the emulator's Rust takes, and the weights add up to the charge. (The host-time
/// profile put a CFrame operation at 350-1,400 ns before CFrame was native.)
#[test]
fn the_work_clock_profile_weighs_stacks_by_the_charge() {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("ree-work-profile-{}-{stamp}", std::process::id()));
    let engine: Rc<Engine> = Engine::new(Options::default()).expect("engine");
    let lua = Lua::new();
    crate::luau_profile::begin_time_at(path.clone(), true, true);
    let guard = crate::luau_profile::ProfileGuard;
    engine_lua::start(&lua, &engine).expect("start");
    crate::luau_profile::install(&lua);
    engine_lua::modules::interpret_all(&lua, &engine);
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        crate::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
    let source = r#"
        local Frame = CFrame.new(1, 2, 3)
        local Sink
        local function Reads()
            for _ = 1, 20000 do
                Sink = Frame * Frame
            end
        end
        local function Adds()
            for Index = 1, 20000 do
                Sink = Index + 1
            end
        end
        Reads()
        Adds()
    "#;
    let function = lua.load(source).set_name("=Test").set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
    if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
        panic!("test script failed: {message}");
    }
    let charged = {
        let state = engine.state.borrow();
        let counters = unsafe { &*state.sched.vm_counters.expect("VM counters") };
        (state.sched.work.get() + state.sched.work_drained.get() + crate::cost::luau(counters)) * 1e6
    };
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("profile");
    let _ = std::fs::remove_file(&path);
    let weight = |name: &str| -> f64 {
        text.lines()
            .filter(|line| line.contains(&format!(";{name} Test:")))
            .map(|line| line.rsplit_once(' ').unwrap().1.parse::<f64>().unwrap())
            .sum()
    };
    let total: f64 = text.lines().map(|line| line.rsplit_once(' ').unwrap().1.parse::<f64>().unwrap()).sum();
    assert!((total - charged).abs() <= charged * 0.02, "{total} microseconds profiled, {charged} charged");
    // a loop step is about 1.5 ns of instructions (cost.rs INTERPRETED_OPS: 0.48 an ADD or FORNLOOP); the product adds the 21 ns of
    // cframe.rs price::MUL_CFRAME: 20-35 ns a step, whatever the host takes
    let (reads, adds) = (weight("Reads") / 20.0, weight("Adds") / 20.0); // nanoseconds a step
    assert!((20.0..35.0).contains(&reads), "a CFrame * CFrame charges {reads} ns\n{text}");
    assert!((1.0..3.0).contains(&adds), "an addition step charges {adds} ns\n{text}");
}

/// CFrame math is native code, as in Roblox: reading a member, calling a method or an operator runs no
/// instruction of the engine's own Luau (it used to run about four per operation: an __index function and a
/// converting wrapper). The work clock charges the script what Studio spends on each (cframe.rs `price`,
/// nanoseconds beyond the instructions that reach it; it charged one or two flat host calls of
/// cost.rs LUAU_HOSTCALL before).
#[test]
fn cframe_operations_run_natively_and_keep_their_work_clock_charge() {
    let engine: Rc<Engine> = Engine::new(Options::default()).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let counters = || {
        let pointer = engine.state.borrow().sched.vm_counters.expect("VM counters");
        let counters = unsafe { &*pointer };
        (counters.hostnanos, crate::cost::host_instructions(counters))
    };
    let run = |loops: u32| {
        let script = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("ModuleScript").unwrap();
            crate::instance::create_named(&mut state, &engine.reflection, class, "Test")
        };
        let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
        let source = format!(
            r#"
            local A = CFrame.new(1, 2, 3) * CFrame.Angles(0.3, 0.4, 0.5)
            local B = CFrame.new(-4.5, 0.25, 6)
            local P = Vector3.new(0.5, -1.25, 2)
            local Sink
            for _ = 1, {loops} do
                Sink = A.Position
                Sink = A.LookVector
                Sink = A * B
                Sink = A * P
                Sink = A + P
                Sink = A:Inverse()
                Sink = A:ToAxisAngle()
                Sink = A:PointToWorldSpace(P)
                Sink = A:ToWorldSpace(B)
                Sink = A:Lerp(B, 0.5)
            end
            "#
        );
        let function = lua.load(source).set_name("=Test").set_environment(env).into_function().expect("compile");
        let before = counters();
        let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
        if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
            panic!("test script failed: {message}");
        }
        let after = counters();
        (after.0 - before.0, after.1 - before.1)
    };
    run(0); // (the libraries resolve their members on first use)
    let (none_calls, none_ops) = run(0);
    let (hundred_calls, hundred_ops) = run(100);
    // 100 rounds of: Position and LookVector (1 each), a product of CFrames (21), of a CFrame and a vector (11), a sum (21),
    // Inverse (26), ToAxisAngle (10), PointToWorldSpace (8), ToWorldSpace (10) and Lerp (66): the parity bench's prices where it has a
    // row outside its held-out third (cframe.rs `price`), the rig frame's for the others
    assert_eq!(hundred_calls - none_calls, 100 * (1 + 1 + 21 + 11 + 21 + 26 + 10 + 8 + 10 + 66));
    assert_eq!(hundred_ops - none_ops, 0, "a CFrame operation ran engine Luau");
}

/// tostring of each datatype, Color3's hex and arithmetic, and the constructors whose results it shows, as a playing client
/// prints them (Studio 0.744, tools/parity/probes/tostring.luau): 9 significant digits for UDim, Vector2, CFrame, Rect, Ray,
/// Region3 and PhysicalProperties, 6 for Color3, NumberRange, the sequences and TweenInfo's times, Faces by NormalId value,
/// Axes by axis only, a DateTime as its milliseconds.
#[test]
fn datatypes_print_as_a_playing_client_does() {
    assert_script(
        r##"
        local function Is(Value, Text)
            local Got = tostring(Value)
            assert(Got == Text, `{Text} printed as {Got}`)
        end
        local Third = 1 / 3
        Is(UDim.new(Third, 7), "0.333333343, 7")
        Is(UDim2.new(Third, 7, -2.5, -9), "{0.333333343, 7}, {-2.5, -9}")
        Is(UDim2.new(1e-7, 0, -0, 1), "{1.00000001e-07, 0}, {-0, 1}")
        Is(Vector2.new(1e-7, 1e20), "1.00000001e-07, 1.00000002e+20")
        Is(Vector3int16.new(1.5, -1.5, 2.7), "1, -1, 2")
        Is(Vector3int16.new(32768, -32769, 65537), "-32768, 32767, 1")
        Is(CFrame.new(Third, -2.5, 1e20), "0.333333343, -2.5, 1.00000002e+20, 1, 0, 0, 0, 1, 0, 0, 0, 1")
        Is(Color3.new(Third, 0.1, 2 / 3), "0.333333, 0.1, 0.666667")
        Is(Color3.new(1e-7, 0.123456789, 1e-20), "1e-07, 0.123457, 1e-20")
        Is(Rect.new(Third, -2.5, 0.1, 1e20), "0.100000001, -2.5, 0.333333343, 1.00000002e+20")
        Is(NumberRange.new(Third, 2.5), "0.333333 2.5 ")
        Is(NumberSequence.new({ NumberSequenceKeypoint.new(0, Third, 0.1), NumberSequenceKeypoint.new(1, -2.5, 0) }), "0 0.333333 0.1 1 -2.5 0 ")
        Is(NumberSequenceKeypoint.new(0.25, Third, 0.5), "0.25 0.333333 0.5 ")
        Is(ColorSequence.new({ ColorSequenceKeypoint.new(0, Color3.new(Third, 0.1, 2 / 3)), ColorSequenceKeypoint.new(1, Color3.new(1, 0, 0.25)) }),
            "0 0.333333 0.1 0.666667 0 1 1 0 0.25 0 ")
        Is(Ray.new(Vector3.new(Third, -2.5, 0.1), Vector3.new(1e20, -0, 1e-7)), "{0.333333343, -2.5, 0.100000001}, {1.00000002e+20, -0, 1.00000001e-07}")
        Is(Region3.new(Vector3.new(-Third, -2.5, 0), Vector3.new(1, 2.25, 0.1)), "0.333333313, -0.125, 0.0500000007, 1, 0, 0, 0, 1, 0, 0, 0, 1; 1.33333337, 4.75, 0.100000001")
        Is(Region3int16.new(Vector3int16.new(-1, 2, 3), Vector3int16.new(4, 5, 6)), "-1, 2, 3; 4, 5, 6")
        Is(TweenInfo.new(0.1, Enum.EasingStyle.Sine, Enum.EasingDirection.In, -1, true, Third), "Time:0.1 DelayTime:0.333333 RepeatCount:-1 Reverses:True EasingDirection:In EasingStyle:Sine")
        Is(Axes.new(Enum.NormalId.Left, Enum.NormalId.Top), "X, Y")
        Is(Faces.new(Enum.NormalId.Top, Enum.NormalId.Left, Enum.NormalId.Back), "Top, Back, Left")
        Is(PhysicalProperties.new(Enum.Material.Wood), "0.349999994, 0.479999989, 0.200000003, 1, 1, 0.25")
        Is(PhysicalProperties.new(Third, 0.1, 0.25, 2, 3), "0.333333343, 0.100000001, 0.25, 2, 3, 1")
        Is(DateTime.fromUnixTimestampMillis(1700000000123), "1700000000123")
        Is(PathWaypoint.new(Vector3.new(Third, 2, 3), Enum.PathWaypointAction.Jump, "label"), "{0.333333343, 2, 3}, Jump, label")
        Is(PathWaypoint.new(), "{0, 0, 0}, Walk, ")
        local Params = RaycastParams.new()
        Params.FilterDescendantsInstances = { workspace }
        Params.IgnoreWater = true
        Is(Params, "RaycastParams{IgnoreWater=true, Margin=0, SizeGradient=0, BruteForceAllSlow=false, RespectCanCollide=false, CollisionGroup=Default, FilterDescendantsInstances={Workspace}}")
        local Overlap = OverlapParams.new()
        Overlap.FilterDescendantsInstances = { workspace, workspace }
        Is(Overlap, "OverlapParams{MaxParts=0, Tolerance=0, BruteForceAllSlow=false, RespectCanCollide=false, CollisionGroup=Default, FilterDescendantsInstances={2 instances...}}")
        Is(CatalogSearchParams.new(), "CatalogSearchParams{SearchKeyword=, CategoryFilter=None, SortType=Relevance, SortAggregation=AllTime, SalesTypeFilter=All, CreatorName=, Limit=30, CreatorType=All, BundleTypes=, AssetTypes=, IncludeOffSale=false}")
        Is(Path2DControlPoint.new(), "Path2DControlPoint { Position = {0, 0}, {0, 0}, LeftTangent = {0, 0}, {0, 0}, RightTangent = {0, 0}, {0, 0} }")
        -- Color3: lower-case hex rounded half away from zero, one message for every malformed hex string, no arithmetic
        local Hexes = {}
        for _, Color in { Color3.new(0.5, 0.5, 0.5), Color3.new(Third, 2 / 3, 1), Color3.new(-0.1, 1.5, 0.5), Color3.new(0.5 / 255, 1.5 / 255, 2.5 / 255) } do
            table.insert(Hexes, Color:ToHex())
        end
        assert(table.concat(Hexes, " ") == "808080 55aaff 00ff80 010203", table.concat(Hexes, " "))
        for _, Text in { "ZZZ", "#12", "1234567", "", " #ABC", "#GGGGGG", "\195\132BC" } do
            local Ok, Message = pcall(Color3.fromHex, Text)
            assert(not Ok and Message == "Unable to convert characters to hex value", tostring(Message))
        end
        for _, Case in { { "add", function(A, B) return A + B end }, { "sub", function(A, B) return A - B end }, { "mul", function(A, B) return A * B end } } do
            local Ok, Message = pcall(Case[2], Color3.new(0.5, 0.25, 1), Color3.new(0.25, 0.5, 0))
            assert(not Ok and string.find(tostring(Message), `attempt to perform arithmetic %({Case[1]}%) on Color3$`), tostring(Message))
        end
        assert(not pcall(function() return -Color3.new() end) and not pcall(function() return Color3.new() * 2 end))
        "##,
    );
}

/// `CFrame:FuzzyEq(other, eps = 1e-5)` answers as a client does (measured in a playing client, Studio 0.744; the parity probe
/// cframe_fuzzyeq keeps the rules): position component by component within `|eps| * (|a| + 1)` of self's component `a`, rotation
/// by `sqrt(sum of (Ra^T Rb - I)^2) / sqrt(2) <= |eps|` in single precision, eps checked to be at most 0.1 in size, and the
/// client's argument errors. The near pairs are ones the client decided where the same formula in double precision, or on
/// the plain difference of the matrices, decides the other way.
#[test]
fn cframe_fuzzy_eq_answers_as_a_client_does() {
    assert_script(
        r##"
        local I = CFrame.identity
        -- position: the default eps is 1e-5 as a float, and the tolerance grows with self's component only
        assert(CFrame.new(1e-5, 0, 0):FuzzyEq(I) and I:FuzzyEq(CFrame.new(0, 1e-5, 0)))
        assert(CFrame.new(1.0000001e-5, 0, 0):FuzzyEq(I), "self's 1e-5 widens its own tolerance")
        assert(not I:FuzzyEq(CFrame.new(0, 1.0000001e-5, 0)), "the other's does not")
        assert(CFrame.new(1, 0, 0):FuzzyEq(CFrame.new(1.125, 0, 0), 0.0625) and not CFrame.new(1, 0, 0):FuzzyEq(CFrame.new(1.125 + 2 ^ -23, 0, 0), 0.0625))
        assert(CFrame.new(1e10, 0, 0):FuzzyEq(CFrame.new(1e10 + 1000, 0, 0), 0.1), "relative to the component's size")
        assert(CFrame.new(math.huge, 0, 0):FuzzyEq(CFrame.new(math.huge, 0, 0)) and not CFrame.new(math.huge, 0, 0):FuzzyEq(I, 0.1))
        assert(CFrame.new(0.0099, 0.0099, 0.0099):FuzzyEq(I, 0.01), "component by component, not a distance")
        assert(not CFrame.new(0 / 0, 0, 0):FuzzyEq(CFrame.new(0 / 0, 0, 0)))
        -- eps: a float, its size, NaN fails, 0 still takes equal CFrames
        assert(CFrame.new(0.01, 0, 0):FuzzyEq(I, 0.0099999997), "0.0099999997 is 0.01 as a float")
        assert(CFrame.new(1e-6, 0, 0):FuzzyEq(I, -0.0001) and I:FuzzyEq(I, 0) and not CFrame.new(1e-6, 0, 0):FuzzyEq(I, 0))
        assert(not I:FuzzyEq(I, 0 / 0) and I:FuzzyEq(I, 0.1000000001) and I:FuzzyEq(I, -0.1))
        -- rotation: the angle between them (2 asin(eps / 2) exactly), and a plain distance for matrices that are not rotations
        local W = 10 ^ 7
        local function Turn(X, Y, Z)
            local N = W * W + X * X + Y * Y + Z * Z
            return CFrame.new(0, 0, 0, (W * W + X * X - Y * Y - Z * Z) / N, 2 * (X * Y - W * Z) / N, 2 * (X * Z + W * Y) / N,
                2 * (X * Y + W * Z) / N, (W * W - X * X + Y * Y - Z * Z) / N, 2 * (Y * Z - W * X) / N,
                2 * (X * Z - W * Y) / N, 2 * (Y * Z + W * X) / N, (W * W - X * X - Y * Y + Z * Z) / N)
        end
        assert(Turn(500623, 0, 0):FuzzyEq(I, 0.1) and not Turn(500628, 0, 0):FuzzyEq(I, 0.1), "0.1 turns at 1.00042 eps")
        assert(I:FuzzyEq(Turn(0, 500623, 0), 0.1) and not I:FuzzyEq(Turn(0, 0, 500628), 0.1), "either way round, any axis")
        assert(CFrame.new(0, 0, 0, 1.141421, 0, 0, 0, 1, 0, 0, 0, 1):FuzzyEq(I, 0.1) and not CFrame.new(0, 0, 0, 1.141422, 0, 0, 0, 1, 0, 0, 0, 1):FuzzyEq(I, 0.1))
        assert(CFrame.new(0, 0, 0, 1, 0.14142, 0, 0, 1, 0, 0, 0, 1):FuzzyEq(I, 0.1) and not CFrame.new(0, 0, 0, 1, 0.141422, 0, 0, 1, 0, 0, 0, 1):FuzzyEq(I, 0.1))
        for _, Case in {
            { false, 0.05, { 1, 0, 0, 0, 1, 0, 0, 0, 1 }, { 0.999878824, -0.00312961265, -0.0152577423, 0.0038508235, 0.998865187, 0.0474706739, 0.0150918644, -0.0475236662, 0.998756111 } },
            { false, 0.05, { 1, 0, 0, 0, 1, 0, 0, 0, 1 }, { 0.999878705, -0.00312961289, -0.0152577423, 0.00385082304, 0.998865187, 0.0474706739, 0.0150918607, -0.0475236699, 0.998756111 } },
            { true, 0.05, { 1, 0, 0, 0, 1, 0, 0, 0, 1 }, { 0.999878526, -0.00312961242, -0.0152577441, 0.0038508235, 0.998865366, 0.0474706739, 0.0150918635, -0.0475236699, 0.998755991 } },
            { true, 0.05, { 1, 0, 0, 0, 1, 0, 0, 0, 1 }, { 0.999878705, -0.00312961242, -0.0152577423, 0.0038508235, 0.998865187, 0.0474706739, 0.0150918635, -0.0475236662, 0.998755991 } },
            { true, 0.05, { -0.0691823885, 0.0880503133, -0.993710697, 0.968553483, -0.232704401, -0.0880503133, -0.238993704, -0.968553483, -0.0691823885 }, { -0.0645823926, 0.137504831, -0.988393366, 0.96857965, -0.229741171, -0.0952492356, -0.240171924, -0.963489175, -0.118347161 } },
            { true, 0.05, { -0.0691823885, 0.0880503133, -0.993710697, 0.968553483, -0.232704401, -0.0880503133, -0.238993704, -0.968553483, -0.0691823885 }, { -0.0645823926, 0.137504846, -0.988393426, 0.968579531, -0.229741171, -0.0952492133, -0.24017188, -0.963489175, -0.118347161 } },
            { false, 0.05, { -0.360000014, -0.864000022, -0.351999998, 0.479999989, 0.151999995, -0.864000022, 0.800000012, -0.479999989, 0.360000014 }, { -0.32706365, -0.871845067, -0.364575863, 0.516429842, 0.158191398, -0.841591179, 0.79140985, -0.463531762, 0.398508191 } },
            { false, 0.05, { -0.360000014, -0.864000022, -0.351999998, 0.479999989, 0.151999995, -0.864000022, 0.800000012, -0.479999989, 0.360000014 }, { -0.32706365, -0.871845067, -0.364575893, 0.516429842, 0.158191383, -0.841591179, 0.79140985, -0.463531703, 0.398508161 } },
        } do
            local A, B = CFrame.new(0, 0, 0, table.unpack(Case[3])), CFrame.new(0, 0, 0, table.unpack(Case[4]))
            -- (as self:FuzzyEq(other): this close, the sum of squares runs over the matrix the other way round from the other side, and can
            -- land a float step apart)
            assert(A:FuzzyEq(B, Case[2]) == Case[1], "a near pair the client decided " .. tostring(Case[1]))
        end
        -- the client's errors: argument errors from the calling line, the count and range ones without it
        local function Fails(Pattern, Function)
            local Ok, Message = pcall(Function)
            assert(not Ok and type(Message) == "string" and string.find(Message, Pattern), tostring(Message))
        end
        Fails("^Invalid number of arguments: 1, CFrame%.FuzzyEq expects 2 or 3%.$", function() return I:FuzzyEq() end)
        Fails("^Invalid number of arguments: 0, CFrame%.FuzzyEq expects 2 or 3%.$", function() return I.FuzzyEq() end)
        Fails("^Test:%d+: invalid argument #1 to 'FuzzyEq' %(CFrame expected, got number%)$", function() return I.FuzzyEq(5, I, "x") end)
        Fails("^Test:%d+: invalid argument #2 %(CFrame expected, got nil%)$", function() return I:FuzzyEq(nil, 5) end)
        Fails("^Test:%d+: invalid argument #2 %(CFrame expected, got Vector3%)$", function() return I:FuzzyEq(Vector3.zero) end)
        Fails("^Test:%d+: invalid argument #2 %(CFrame expected, got Instance%)$", function() return I:FuzzyEq(Instance.new("Folder")) end)
        Fails("^Test:%d+: invalid argument #3 %(number expected, got nil%)$", function() return I:FuzzyEq(I, nil) end)
        Fails("^Test:%d+: invalid argument #3 %(number expected, got string%)$", function() return I:FuzzyEq(I, "abc") end)
        Fails("^The eps value provided to FuzzyEq should be a small positive value <= 0%.1$", function() return I:FuzzyEq(I, "5") end)
        Fails("^The eps value provided to FuzzyEq should be a small positive value <= 0%.1$", function() return I:FuzzyEq(I, -0.10000001) end)
        Fails("^The eps value provided to FuzzyEq should be a small positive value <= 0%.1$", function() return I:FuzzyEq(I, math.huge) end)
        assert(CFrame.new(1e-6, 0, 0):FuzzyEq(I, "0.01") and I:FuzzyEq(I, 0.01, "extra") and select("#", I:FuzzyEq(I)) == 1)
        assert(CFrame.FuzzyEq == nil and type(I.FuzzyEq) == "function")
        "##,
    );
}
