-- FloatCurve: the tangent a Cubic key takes when none is set, between keys of every proportion. Runs unchanged in
-- Studio (the command bar or a plugin) and here; tangents.txt is what Studio 0.747 returned (2026-11-11). Every number
-- is compared digit for digit.
local out = {}
local function S(v) return string.format("%.9g", v) end
local K = Enum.KeyInterpolationMode.Cubic
-- (a generator of its own, so that both sides build the same keys)
local seed = 12345
local function random(low, high)
	seed = (seed * 1103515245 + 12345) % 2147483648
	return low + (high - low) * (seed / 2147483648)
end
local function tick(t) return math.floor(t * 2400 + 0.5) / 2400 end
for case = 1, 90 do
	local count = if case % 5 == 0 then 5 else 3
	local time, value = tick(random(-2, 2)), random(-5, 5)
	local sign = if case % 2 == 0 then 1 else -1
	local keys, times = {}, {}
	for index = 1, count do
		table.insert(keys, FloatCurveKey.new(time, value, K))
		table.insert(times, time)
		local step = random(0.01, 10)
		if (case + index) % 3 == 0 then step *= random(0.001, 0.4) end
		-- (now and then a peak, a dip or a level step)
		if case % 11 == 0 and index == 2 then sign = -sign end
		if case % 13 == 0 and index == 1 then step = 0 end
		time, value = tick(time + random(0.1, 3)), value + sign * step
	end
	local curve = Instance.new("FloatCurve")
	curve:SetKeys(keys)
	local row = {}
	for index = 1, count - 1 do
		for part = 1, 3 do table.insert(row, S(curve:GetValueAtTime(times[index] + (times[index + 1] - times[index]) * part / 4))) end
	end
	table.insert(out, case .. " " .. table.concat(row, " "))
end
return table.concat(out, "\n")
