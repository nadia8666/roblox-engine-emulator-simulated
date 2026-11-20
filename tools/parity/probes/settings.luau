-- What a playing client's settings and input services start with (UserGameSettings: a fresh client's settings file).
local Settings = UserSettings():GetService("UserGameSettings")
local Input = game:GetService("UserInputService")
for _, Name in { "MouseSensitivity", "GamepadCameraSensitivity", "RotationType", "ComputerMovementMode", "ComputerCameraMovementMode", "SavedQualityLevel" } do
	print("UserGameSettings." .. Name .. " " .. P.try(function()
		return tostring(Settings[Name])
	end))
end
print("UserInputService.MouseDeltaSensitivity " .. P.try(function()
	return Input.MouseDeltaSensitivity
end))
