-- A 60-second, 60-fps sampling check with a known number of identical loop iterations. Run with --script-timeout 0
-- and --profile hw --profile-counters cycles,instructions. Steps sets the work per call (default 9000), Background
-- adds idle task.wait loops, Recursive uses four coroutines each recursing four deep, and Bridge adds a thin pcall
-- wrapper. Compare callee's (or recursive's) own instructions with reference's times MEASURE's calls * steps / 1000000.
-- Table fields keep Luau's optimizer from inlining the measured functions into their callers.
local FRAMES = 3600 -- 60 simulated seconds at 60 frames a second
local COROUTINES = 4
local RECURSION_DEPTH = 3 -- a recursive call makes this many more calls below it, four in all

return function(Context)
	local work = {}
	function work.reference(x, steps)
		for _ = 1, steps do
			x = (x * 1664525 + 1013904223) % 4294967296
		end
		return x
	end
	function work.callee(x, steps)
		for _ = 1, steps do
			x = (x * 1664525 + 1013904223) % 4294967296
		end
		return x
	end
	function work.recursive(x, steps, depth)
		for _ = 1, steps do
			x = (x * 1664525 + 1013904223) % 4294967296
		end
		if depth > 0 then
			x = work.recursive(x, steps, depth - 1)
		end
		return x
	end
	function work.bridge(x, steps)
		local ok, result = pcall(function()
			return work.callee(x, steps)
		end)
		assert(ok, result)
		return result
	end
	Context.ProfilePhase("reference")
	work.reference(1, 1000000)
	Context.ProfilePhase("frames")
	local steps = Context.Steps or 9000
	for _ = 1, Context.Background or 0 do
		task.spawn(function()
			while true do
				task.wait()
			end
		end)
	end
	local calls = 0
	local x = 1
	if Context.Recursive then
		local callsPerRecursion = RECURSION_DEPTH + 1
		local totalCalls = COROUTINES * FRAMES * callsPerRecursion
		for _ = 1, COROUTINES do
			task.spawn(function()
				for _ = 1, FRAMES do
					x = work.recursive(x, steps, RECURSION_DEPTH)
					calls += callsPerRecursion
					if calls == totalCalls then
						print("MEASURE", calls, steps, x)
					end
					task.wait()
				end
			end)
		end
	else
		game:GetService("RunService").Heartbeat:Connect(function()
			if calls == FRAMES then
				return
			end
			if Context.Bridge then
				x = work.bridge(x, steps)
			else
				x = work.callee(x, steps)
			end
			calls += 1
			if calls == FRAMES then
				print("MEASURE", calls, steps, x)
			end
		end)
	end
end
