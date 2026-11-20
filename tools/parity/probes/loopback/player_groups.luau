-- parity: loopback
-- Player's group methods on each side of a play session (the client's LocalPlayer, the player who joined the server): the
-- Async ones (IsInGroupAsync, GetRankInGroupAsync, GetRoleInGroupAsync) beside the older IsInGroup, GetRankInGroup and
-- GetRoleInGroup. Whether each is a member, what it answers for a group id, when a call yields (a thread deferred before
-- it has run when it returns), which group id an argument names, and what the arguments raise. A live game's server
-- calls two of them as the player joins, in PlayerAdded (public issue 19).
local Names = { "IsInGroupAsync", "GetRankInGroupAsync", "GetRoleInGroupAsync", "IsInGroup", "GetRankInGroup", "GetRoleInGroup" }
-- (the member is read inside the pcall: an engine without it raises there)
local function Invoke(Player, Name, Self, ...)
	return Player[Name](Self, ...)
end
local function Call(Player, Name, ...)
	local Ran = false
	task.defer(function()
		Ran = true
	end)
	local Answer = P.try(Invoke, Player, Name, Player, ...)
	return Answer .. (if Ran then " | yielded" else "")
end
local function Groups(Player)
	for _, Name in Names do
		print(Name .. " read " .. P.try(function()
			return typeof(Player[Name])
		end))
	end
	-- each method with a group id no call has named yet, then again
	for Index, Name in Names do
		print("fresh " .. Name .. " " .. Call(Player, Name, 100 + Index) .. " / again " .. Call(Player, Name, 100 + Index))
	end
	-- one method names a group, then the others
	for Index, First in Names do
		local Parts = { First .. " " .. Call(Player, First, 200 + Index) }
		for _, Name in Names do
			if Name ~= First then
				table.insert(Parts, Name .. " " .. Call(Player, Name, 200 + Index))
			end
		end
		print("shared " .. table.concat(Parts, " / "))
	end
	-- the group an argument names: a value no call has named yet, then the ids it might stand for
	local Keys = {
		{ "11.5", 11.5, { 11, 12 } },
		{ "-21.5", -21.5, { -21, -22 } },
		{ "31.4", 31.4, { 31 } },
		{ "true", true, { 1 } },
		{ "false", false, { 0 } },
		{ "nan", 0 / 0, { 0 } },
		{ "inf", math.huge, { 0 } },
		{ "-inf", -math.huge, { 0 } },
		{ "2^63", 2 ^ 63, { 0, -2 ^ 63 } },
		{ "1e20", 1e20, {} },
		{ "-1e20", -1e20, {} },
		{ "2^53", 2 ^ 53, { 2 ^ 53 } },
		{ "'41'", "41", { 41 } },
		{ "'-42'", "-42", { -42 } },
		{ "'+43'", "+43", { 43 } },
		{ "' 44'", " 44", { 44 } },
		{ "'45 '", "45 ", { 45 } },
		{ "'046'", "046", { 46 } },
		{ "'47.0'", "47.0", { 47 } },
		{ "'4.8e1'", "4.8e1", { 48 } },
		{ "'51.5'", "51.5", { 51, 52 } },
		{ "'0x3d'", "0x3d", { 61 } },
		{ "'9007199254740993'", "9007199254740993", { 2 ^ 53 } },
		{ "'9223372036854775807'", "9223372036854775807", {} },
		{ "'9223372036854775808'", "9223372036854775808", {} },
	}
	for _, Key in Keys do
		local Parts = { Key[1] .. " " .. Call(Player, "IsInGroupAsync", Key[2]) }
		for _, Id in Key[3] do
			table.insert(Parts, P.num(Id) .. " " .. Call(Player, "IsInGroupAsync", Id))
		end
		print("key " .. table.concat(Parts, " / "))
	end
	-- arguments
	local Cases = {
		{ "7", 7 },
		{ "-1", -1 },
		{ "7.5", 7.5 },
		{ "'7'", "7" },
		{ "'abc'", "abc" },
		{ "''", "" },
		{ "true", true },
		{ "{}", {} },
		{ "{1}", { 1 } },
		{ "{a=1}", { a = 1 } },
		{ "Vector3", Vector3.zero },
		{ "function", print },
		{ "workspace", workspace },
		{ "Enum", Enum.KeyCode.A },
	}
	for _, Name in Names do
		local Parts = {}
		for _, Case in Cases do
			table.insert(Parts, Case[1] .. " " .. P.try(Invoke, Player, Name, Player, Case[2]))
		end
		table.insert(Parts, "nil " .. P.try(Invoke, Player, Name, Player, nil))
		table.insert(Parts, "none " .. P.try(Invoke, Player, Name, Player))
		table.insert(Parts, "7,extra " .. P.try(Invoke, Player, Name, Player, 7, "extra"))
		table.insert(Parts, "other-self " .. P.try(Invoke, Player, Name, workspace, 7))
		table.insert(Parts, "no-self " .. P.try(Invoke, Player, Name, nil, 7))
		print("args " .. Name .. " " .. table.concat(Parts, " | "))
	end
	print("same " .. P.try(function()
		return Player.IsInGroupAsync == Player.IsInGroupAsync, Player.IsInGroupAsync == Player.IsInGroup
	end))
end
--- server
local Players = game:GetService("Players")
local Joined
Players.PlayerAdded:Connect(function(Player)
	local InGroup = Call(Player, "IsInGroupAsync", 7)
	local Rank = Call(Player, "GetRankInGroupAsync", 7)
	Joined = "player-added in-group " .. InGroup .. " rank " .. Rank
end)
local Player = P.player()
P.meet("joined")
local Deadline = os.clock() + 30
while not Joined and os.clock() < Deadline do
	task.wait()
end
print(Joined or "player-added never answered")
Groups(Player)
--- client
P.meet("joined")
Groups(P.player())
