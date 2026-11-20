-- Engine members only Roblox's own scripts may call: the fast-flag methods of DataModel and GuiService's
-- GetResolutionScale. What a game script sees when it reads one (the member is there) and when it calls one: the
-- error, caught by pcall, in each call form, and a settings helper that falls back on its default when the call raises.
-- Run on a client and on a server (--server).
local GuiService = game:GetService("GuiService")
local Calls = {
	{ "DefineFastFlag", game, function() return game:DefineFastFlag("ParityProbeFlag", true) end },
	{ "DefineFastInt", game, function() return game:DefineFastInt("ParityProbeInt", 7) end },
	{ "DefineFastString", game, function() return game:DefineFastString("ParityProbeString", "seven") end },
	{ "GetFastFlag", game, function() return game:GetFastFlag("ParityProbeFlag") end },
	{ "GetFastInt", game, function() return game:GetFastInt("ParityProbeInt") end },
	{ "GetFastString", game, function() return game:GetFastString("ParityProbeString") end },
	{ "GetEngineFeature", game, function() return game:GetEngineFeature("ParityProbeFeature") end },
	{ "GetResolutionScale", GuiService, function() return GuiService:GetResolutionScale() end },
}
for _, Call in Calls do
	local Name, Owner, Colon = Call[1], Call[2], Call[3]
	local Read = P.try(function()
		return typeof(Owner[Name])
	end)
	local Bare = P.try(function()
		return Owner[Name](Owner)
	end)
	local Typed = P.try(function()
		return Owner[Name](Owner, {}, {})
	end)
	local Caught = P.try(function()
		local Ok, Message = pcall(Colon)
		return Ok, type(Message)
	end)
	print(Name .. " read " .. Read .. " | call " .. P.try(Colon) .. " | no-args " .. Bare .. " | tables " .. Typed .. " | caught " .. Caught)
end
-- the method of one class called on another instance, and with no instance at all
print("other-self " .. P.try(function()
	return game.GetFastInt(workspace, "ParityProbeInt")
end) .. " | " .. P.try(function()
	return GuiService.GetResolutionScale(game)
end))
print("no-self " .. P.try(function()
	return game.DefineFastInt("ParityProbeInt", 7)
end) .. " | " .. P.try(function()
	return GuiService.GetResolutionScale()
end))
-- the method is one function each time it is read, on every instance of the class
print("same " .. tostring(game.GetFastFlag == game.GetFastFlag) .. " " .. tostring(game.DefineFastInt == game.GetFastInt))

-- A settings helper as a UI library has one: define the flag, and take the default when the call raises
local function Define(Kind, Name, Default)
	local Ok, Value = pcall(function()
		return game["Define" .. Kind](game, Name, Default)
	end)
	if not Ok then
		return Default, "default"
	end
	return Value, "defined"
end
for _, Case in { { "FastInt", "ParityProbeFrameMs", 5 }, { "FastFlag", "ParityProbeEnabled", true }, { "FastString", "ParityProbeMode", "on" } } do
	local Value, How = Define(Case[1], Case[2], Case[3])
	print("define " .. Case[1] .. " " .. P.num(Value) .. " " .. How)
end
-- the default then divides as a scheduler's frame budget does
local Interval = Define("FastInt", "ParityProbeYieldInterval", 5)
print("frame " .. P.try(function()
	return 1000 / Interval
end))
