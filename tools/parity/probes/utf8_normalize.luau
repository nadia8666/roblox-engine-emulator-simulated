-- utf8.nfcnormalize and utf8.nfdnormalize: what each answers for precomposed and decomposed Latin, stacked marks in
-- either order, marks that do not compose, singletons, compatibility characters, Hangul syllables and jamo, composition
-- exclusions, CJK compatibility ideographs, characters of later Unicode versions, invalid UTF-8 and arguments that are
-- not strings, printed as code points; then a scan of every code point below U+30000 through both functions, as counts
-- and a hash, so the whole table is compared.
local U = utf8.char

local function Hex(Text)
	if Text == "" then
		return "-"
	end
	if utf8.len(Text) == nil then
		local Bytes = {}
		for Index = 1, #Text do
			Bytes[Index] = string.format("%02X", string.byte(Text, Index))
		end
		return "bytes " .. table.concat(Bytes, " ")
	end
	local Codes = {}
	for _, Code in utf8.codes(Text) do
		table.insert(Codes, string.format("%04X", Code))
	end
	return table.concat(Codes, " ")
end

local function Show(Ok, ...)
	local Value = ...
	if not Ok then
		return "error " .. P.err(Value)
	end
	local Count = select("#", ...)
	local Text = if type(Value) == "string" then Hex(Value) else "<" .. type(Value) .. "> " .. tostring(Value)
	return if Count == 1 then Text else Text .. " (" .. Count .. " values)"
end

local function Both(Label, Text)
	print(Label .. " nfc " .. Show(pcall(utf8.nfcnormalize, Text)) .. " nfd " .. Show(pcall(utf8.nfdnormalize, Text)))
end

local Cases = {
	{ "empty", "" },
	{ "ascii", "abc" },
	{ "nul-then-mark", "a\0" .. U(0x301) },
	-- Latin
	{ "e-acute", U(0xE9) },
	{ "e+acute", U(0x65, 0x301) },
	{ "A-ring", U(0xC5) },
	{ "A+ring", U(0x41, 0x30A) },
	{ "A-ring+acute", U(0xC5, 0x301) },
	{ "u-diaeresis-macron", U(0x1D6) },
	{ "u+diaeresis+macron", U(0x75, 0x308, 0x304) },
	{ "u-diaeresis+macron", U(0xFC, 0x304) },
	{ "e-dotbelow-circumflex", U(0x1EC7) },
	{ "two-letters", U(0x65, 0x301, 0x65, 0x301) },
	{ "mark-first", U(0x301, 0x65) },
	{ "acute+acute", U(0x61, 0x301, 0x301) },
	-- stacked marks in either order (dot below is class 220, circumflex and acute 230, horn 216)
	{ "a+dotbelow+circumflex", U(0x61, 0x323, 0x302) },
	{ "a+circumflex+dotbelow", U(0x61, 0x302, 0x323) },
	{ "a-dotbelow+circumflex", U(0x1EA1, 0x302) },
	{ "a-circumflex+dotbelow", U(0xE2, 0x323) },
	{ "a-dotbelow-circumflex", U(0x1EAD) },
	{ "a+acute+dotbelow", U(0x61, 0x301, 0x323) },
	{ "o+horn+acute", U(0x6F, 0x31B, 0x301) },
	{ "o+acute+horn", U(0x6F, 0x301, 0x31B) },
	-- a mark that composes with nothing, then one that would compose with the letter
	{ "a+gravebelow+acute", U(0x61, 0x316, 0x301) },
	{ "a+diaeresis+acute", U(0x61, 0x308, 0x301) },
	{ "a+diaeresis+macron", U(0x61, 0x308, 0x304) },
	{ "e+cedilla+breve", U(0x65, 0x327, 0x306) },
	{ "e+breve+cedilla", U(0x65, 0x306, 0x327) },
	-- Greek
	{ "alpha+psili+varia+ypogegrammeni", U(0x3B1, 0x313, 0x300, 0x345) },
	{ "alpha+ypogegrammeni+psili+varia", U(0x3B1, 0x345, 0x313, 0x300) },
	{ "alpha-psili-varia-ypogegrammeni", U(0x1F82) },
	{ "iota-dialytika-tonos", U(0x390) },
	{ "alpha-oxia", U(0x1F71) },
	{ "alpha-oxia+ypogegrammeni", U(0x1F71, 0x345) },
	-- singletons: characters whose canonical decomposition is one code point
	{ "angstrom", U(0x212B) },
	{ "angstrom+acute", U(0x212B, 0x301) },
	{ "ohm", U(0x2126) },
	{ "kelvin", U(0x212A) },
	{ "en-quad", U(0x2000) },
	{ "em-quad", U(0x2001) },
	{ "grave-tone-mark", U(0x340) },
	{ "acute-tone-mark", U(0x341) },
	{ "koronis", U(0x343) },
	{ "greek-question-mark", U(0x37E) },
	{ "ano-teleia", U(0x387) },
	{ "singleton-then-letter", U(0x212B, 0x62) },
	{ "letter-then-singleton", U(0x62, 0x212B) },
	{ "two-singletons", U(0x2126, 0x2126) },
	{ "singleton+mark", U(0x2126, 0x301) },
	-- CJK compatibility ideographs (singletons too)
	{ "cjk-compat-F900", U(0xF900) },
	{ "cjk-compat-FA10", U(0xFA10) },
	{ "cjk-compat-2F800", U(0x2F800) },
	{ "cjk-compat-2FA1D", U(0x2FA1D) },
	-- compatibility characters: canonical normalization leaves them
	{ "fi-ligature", U(0xFB01) },
	{ "circled-one", U(0x2460) },
	{ "one-half", U(0xBD) },
	{ "superscript-two", U(0xB2) },
	{ "fullwidth-A", U(0xFF21) },
	{ "fullwidth-A+ring", U(0xFF21, 0x30A) },
	{ "no-break-space", U(0xA0) },
	{ "long-s-dot", U(0x1E9B) },
	{ "long-s+dot", U(0x17F, 0x307) },
	-- Hangul
	{ "hangul-ga", U(0xAC00) },
	{ "hangul-gak", U(0xAC01) },
	{ "hangul-last", U(0xD7A3) },
	{ "jamo-L+V", U(0x1100, 0x1161) },
	{ "jamo-L+V+T", U(0x1100, 0x1161, 0x11A8) },
	{ "jamo-last-L+V+T", U(0x1112, 0x1175, 0x11C2) },
	{ "syllable-LV+T", U(0xAC00, 0x11A8) },
	{ "syllable-LVT+T", U(0xAC01, 0x11A8) },
	{ "syllable-LV+T-base", U(0xAC00, 0x11A7) },
	{ "syllable-LV+T-last", U(0xAC00, 0x11C2) },
	{ "jamo-L+old-V", U(0x1100, 0x1176) },
	{ "jamo-old-L+V", U(0x1113, 0x1161) },
	{ "jamo-L+L+V", U(0x1100, 0x1100, 0x1161) },
	{ "jamo-L+mark+V", U(0x1100, 0x301, 0x1161) },
	{ "jamo-V+T", U(0x1161, 0x11A8) },
	{ "jamo-L+V+T+T", U(0x1100, 0x1161, 0x11A8, 0x11A8) },
	{ "jamo-L+V+T-base+T", U(0x1100, 0x1161, 0x11A7, 0x11A8) },
	{ "syllable-LVT+T-base", U(0xAC01, 0x11A7) },
	{ "syllable-T27+T", U(0xAC1B, 0x11A8) },
	{ "syllable-last+T", U(0xD7A3, 0x11A8) },
	{ "syllable-last+T+T", U(0xD7A3, 0x11A8, 0x11A8) },
	{ "syllable-LV+old-T", U(0xAC00, 0x11C3) },
	{ "syllable-LV+V", U(0xAC00, 0x1161) },
	{ "jamo-L+filler-V", U(0x1100, 0x1160) },
	{ "jamo-filler-L+V", U(0x115F, 0x1161) },
	{ "compat-jamo", U(0x3131, 0x314F) },
	-- composition exclusions
	{ "devanagari-qa", U(0x958) },
	{ "devanagari-ka+nukta", U(0x915, 0x93C) },
	{ "hebrew-yod-hiriq", U(0xFB1D) },
	{ "hebrew-yod+hiriq", U(0x5D9, 0x5B4) },
	{ "forking", U(0x2ADC) },
	{ "forking-parts", U(0x2ADD, 0x338) },
	{ "dialytika-tonos-mark", U(0x344) },
	{ "diaeresis+acute-marks", U(0x308, 0x301) },
	{ "tibetan-F73", U(0xF73) },
	{ "tibetan-F71+F72", U(0xF71, 0xF72) },
	{ "musical-1D15E", U(0x1D15E) },
	{ "musical-1D157+1D165", U(0x1D157, 0x1D165) },
	-- two starters that compose
	{ "oriya-B47+B3E", U(0xB47, 0xB3E) },
	{ "kannada-CC6+CC2", U(0xCC6, 0xCC2) },
	{ "kannada-CC6+CC2+CD5", U(0xCC6, 0xCC2, 0xCD5) },
	-- canonical pairs by the Unicode version that added them
	{ "u5.2-kaithi-11099+110BA", U(0x11099, 0x110BA) },
	{ "u6.1-chakma-11131+11127", U(0x11131, 0x11127) },
	{ "u7.0-grantha-11347+1133E", U(0x11347, 0x1133E) },
	{ "u7.0-tirhuta-114B9+114BA", U(0x114B9, 0x114BA) },
	{ "u13.0-dives-akuru-11935+11930", U(0x11935, 0x11930) },
	{ "u13.0-dives-akuru-11938", U(0x11938) },
	{ "u16.0-todhri-105D2+307", U(0x105D2, 0x307) },
	{ "u16.0-todhri-105C9", U(0x105C9) },
	{ "u16.0-gurung-khema-16D67+16D67", U(0x16D67, 0x16D67) },
	-- other planes and edges
	{ "private-use", U(0xE000) },
	{ "noncharacter", U(0xFFFF) },
	{ "last-code-point", U(0x10FFFF) },
	{ "emoji+mark", U(0x1F600, 0x301) },
	-- what nfd writes, read again
	{ "nfd-angstrom-written", U(0x41, 0x30A, 0) },
	{ "nul+acute", U(0, 0x301) },
	{ "A+ring+nul+acute", U(0x41, 0x30A, 0, 0x301) },
	-- invalid UTF-8
	{ "bad-FF", "\255" },
	{ "bad-continuation", "a\128b" },
	{ "bad-truncated", "\195" },
	{ "bad-truncated-mark", "e\204" },
	{ "bad-overlong", "\192\175" },
	{ "bad-surrogate", "\237\160\128" },
	{ "bad-above-max", "\244\144\128\128" },
	{ "bad-after-letter+mark", "e" .. U(0x301) .. "\255" },
	{ "bad-between-letter-and-mark", "e\255" .. U(0x301) },
	{ "bad-after-angstrom", U(0x212B) .. "\255" },
}

for _, Case in Cases do
	Both(Case[1], Case[2])
end

-- the result read again through the other function
for _, Text in { U(0x212B), U(0x1F71), U(0xF900), U(0x61, 0x302, 0x323), U(0xAC01), U(0x958), U(0x344) } do
	print("nfc-of-nfd " .. Hex(Text) .. " -> " .. Hex(utf8.nfcnormalize(utf8.nfdnormalize(Text))) .. " nfd-of-nfc -> " .. Hex(utf8.nfdnormalize(utf8.nfcnormalize(Text))))
end

-- arguments
print("arg-none nfc " .. Show(pcall(utf8.nfcnormalize)) .. " nfd " .. Show(pcall(utf8.nfdnormalize)))
Both("arg-nil", nil)
Both("arg-integer", 123)
Both("arg-float", -0.5)
Both("arg-boolean", true)
Both("arg-table", {})
Both("arg-function", print)
Both("arg-buffer", buffer.fromstring("e\204\129"))
Both("arg-thread", coroutine.create(print))
Both("arg-vector3", Vector3.new(1, 2, 3))
Both("arg-color3", Color3.new(1, 0, 0))
Both("arg-instance", workspace)
print("arg-extra nfc " .. Show(pcall(utf8.nfcnormalize, U(0x65, 0x301), 5)) .. " nfd " .. Show(pcall(utf8.nfdnormalize, U(0xE9), "x")))
print("arg-direct " .. P.try(function()
	return utf8.nfcnormalize(nil)
end) .. " / " .. P.try(function()
	return utf8.nfdnormalize({})
end))
print("bad-direct " .. P.try(function()
	return utf8.nfcnormalize("\255")
end) .. " / " .. P.try(function()
	return utf8.nfdnormalize("e\204")
end))
print("type " .. type(utf8.nfcnormalize) .. " " .. type(utf8.nfdnormalize) .. " " .. tostring(utf8.nfcnormalize == utf8.nfdnormalize))

-- long inputs
local Long = string.rep(U(0x65, 0x301), 20000)
local Composed = utf8.nfcnormalize(Long)
print("long-nfc bytes " .. #Composed .. " codes " .. tostring(utf8.len(Composed)) .. " same " .. tostring(Composed == string.rep(U(0xE9), 20000)))
local Decomposed = utf8.nfdnormalize(string.rep(U(0x212B), 20000))
print("long-nfd bytes " .. #Decomposed .. " codes " .. tostring(utf8.len(Decomposed)) .. " same " .. tostring(Decomposed == string.rep(U(0x41, 0x30A, 0), 20000)))

-- every code point below U+30000 (no later one has a canonical decomposition) through both functions: how many change,
-- an FNV-1a hash of each changed one's code point and result, and the same for nfc of each nfd result
local function Mix(Hash, Text)
	for Index = 1, #Text do
		Hash = bit32.bxor(Hash, string.byte(Text, Index))
		Hash = bit32.band(Hash * 16777619, 0xFFFFFFFF)
	end
	return Hash
end
local Changed, ChangedHash, Composes, RoundHash, Hangul = 0, 2166136261, 0, 2166136261, 0
local RoundSame = 0
for Code = 0, 0x2FFFF do
	if Code >= 0xD800 and Code <= 0xDFFF then
		continue
	end
	if Code % 16384 == 0 then
		task.wait()
	end
	local Text = U(Code)
	local Decomposed = utf8.nfdnormalize(Text)
	if utf8.nfcnormalize(Text) ~= Text then
		Composes += 1
	end
	if Decomposed ~= Text then
		Changed += 1
		if Code >= 0xAC00 and Code <= 0xD7A3 then
			Hangul += 1
		end
		ChangedHash = Mix(ChangedHash, string.format("%X:", Code) .. Decomposed)
		local Round = utf8.nfcnormalize(Decomposed)
		if Round == Text then
			RoundSame += 1
		end
		RoundHash = Mix(RoundHash, string.format("%X:", Code) .. Round)
	end
end
print(string.format("scan nfd changes %d (hangul %d) hash %08X; nfc changes %d", Changed, Hangul, ChangedHash, Composes))
print(string.format("scan nfc of nfd gives back %d hash %08X", RoundSame, RoundHash))
