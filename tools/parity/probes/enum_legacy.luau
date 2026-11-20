-- Enum items' legacy names (the names Roblox's API dump lists as an item's LegacyNames): what a legacy name resolves to,
-- the name and value that item reports, and that the enumeration, FromName and FromValue know the current name only.
local Legacy = {
	{ "AccessoryType", "TeeShirt" },
	{ "AssetType", "TeeShirt" },
	{ "AssetType", "TeeShirtAccessory" },
	{ "AvatarAssetType", "TeeShirtAccessory" },
	{ "Axis", "Left" },
	{ "Axis", "Right" },
	{ "Axis", "Bottom" },
	{ "Axis", "Top" },
	{ "Axis", "Back" },
	{ "Axis", "Front" },
	{ "BinType", "Laser" },
	{ "BinType", "Rocket" },
	{ "BinType", "Slingshot" },
	{ "CatalogSortType", "RecentlyUpdated" },
	{ "ConnectionError", "DisconnectNoResponse" },
	{ "ContextActionPriority", "Default" },
	{ "ControlMode", "Mouse Lock Switch" },
	{ "DeveloperMemoryTag", "PhysicsParts" },
	{ "ExplosionType", "CratersAndDebris" },
	{ "ExternalEditorMode", "CustomEditor" },
	{ "FluidForces", "Enabled" },
	{ "Font", "Montserrat" },
	{ "Font", "GothamSemibold" },
	{ "Font", "MontserratMedium" },
	{ "Font", "MontserratBold" },
	{ "Font", "MontserratBlack" },
	{ "FormFactor", "Block" },
	{ "InputType", "Action1" },
	{ "InputType", "Action2" },
	{ "InputType", "Action3" },
	{ "InputType", "Action4" },
	{ "InputType", "Action5" },
	{ "InputType", "LeftTread" },
	{ "InputType", "RightTread" },
	{ "InputType", "Steer" },
	{ "InputType", "Throtle" },
	{ "InputType", "Throttle" },
	{ "InputType", "UpDown" },
	{ "KeyCode", "Unknown" },
	{ "KeyCode", "Touch" },
	{ "Material", "Corroded Metal" },
	{ "Material", "Aluminum" },
	{ "ModelLevelOfDetail", "Deactivated" },
	{ "QualityLevel", "Level  1" },
	{ "QualityLevel", "Level  2" },
	{ "QualityLevel", "Level  3" },
	{ "QualityLevel", "Level  4" },
	{ "QualityLevel", "Level  5" },
	{ "QualityLevel", "Level  6" },
	{ "QualityLevel", "Level  7" },
	{ "QualityLevel", "Level  8" },
	{ "QualityLevel", "Level  9" },
	{ "QualityLevel", "Level 10" },
	{ "QualityLevel", "Level 11" },
	{ "QualityLevel", "Level 12" },
	{ "QualityLevel", "Level 13" },
	{ "QualityLevel", "Level 14" },
	{ "QualityLevel", "Level 15" },
	{ "QualityLevel", "Level 16" },
	{ "QualityLevel", "Level 17" },
	{ "QualityLevel", "Level 18" },
	{ "QualityLevel", "Level 19" },
	{ "QualityLevel", "Level 20" },
	{ "QualityLevel", "Level 21" },
	{ "RaycastFilterType", "Blacklist" },
	{ "RaycastFilterType", "Whitelist" },
	{ "Style", "Alternating Supports" },
	{ "Style", "Bridge Style Supports" },
	{ "Style", "No Supports" },
	{ "SurfaceType", "Spawn" },
	{ "SurfaceType", "Unjoinable" },
	{ "SurfaceType", "Bumps" },
	{ "ThreadPoolConfig", "PartialThread" },
}
for _, Pair in Legacy do
	local EnumName, Name = Pair[1], Pair[2]
	print("legacy " .. EnumName .. " [" .. Name .. "] " .. P.try(function()
		local Item = (Enum :: any)[EnumName][Name]
		local Listed = false
		for _, Other in (Enum :: any)[EnumName]:GetEnumItems() do
			if Other.Name == Name then
				Listed = true
			end
		end
		return Item.Name, Item.Value, tostring(Item), Listed
	end))
end
print("keycode zero " .. P.try(function()
	local Unknown, None = (Enum.KeyCode :: any).Unknown, (Enum.KeyCode :: any).None
	return Unknown == None, Unknown.Name, None.Name, tostring(Unknown), Enum.KeyCode:GetEnumItems()[1].Name, Enum.KeyCode:GetEnumItems()[1].Value
end))
print("fromname " .. P.try(function() return tostring((Enum.KeyCode :: any):FromName("Unknown")), tostring((Enum.KeyCode :: any):FromName("None")) end))
print("fromvalue " .. P.try(function() return tostring((Enum.KeyCode :: any):FromValue(0)) end))
print("missing " .. P.try(function() return (Enum.KeyCode :: any).NoSuchKey end))
