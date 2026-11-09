-- The hardware counters' check of attribution: a function that waits on memory (chasing a random cycle through 64 MB
-- of array slots, far more than the L3) called between calls of one that computes (multiply-adds on one number, no
-- memory beyond the VM's own). The cycles stalled on loads must land on the chases, not on the computes, both when
-- every call is read around (calls longer than the threshold) and when one call in several is (shorter calls). Run it
-- with
--   ree run --harness tools/hw-counters/attribution.luau --profile hw --seconds 0.1 --profile-out <dir>
-- and read the summary's top functions (docs/cli.md "Hardware counters", checking it).
--
-- (the functions are a table's fields: Luau's -O2 inlines a small local function into its caller, and an inlined call
-- has no frame to read around, so its counts are its caller's)
return function(Context)
	-- a random cycle through 4M slots: Sattolo's shuffle of the identity (one cycle through every slot), with a
	-- multiply-add generator (cheaper than a method call a slot)
	local count = 4 * 1024 * 1024
	local nextOf = table.create(count, 0)
	for i = 1, count do
		nextOf[i] = i
	end
	local seed = 12345
	for i = count, 2, -1 do
		seed = (seed * 1103515245 + 12345) % 2147483648
		local j = seed % (i - 1) + 1
		nextOf[i], nextOf[j] = nextOf[j], nextOf[i]
	end

	local work = {}
	function work.chaseLong(at, steps)
		for _ = 1, steps do
			at = nextOf[at]
		end
		return at
	end
	function work.computeLong(x, steps)
		for _ = 1, steps do
			x = (x * 1103515245 + 12345) % 2147483648
		end
		return x
	end
	function work.chaseShort(at, steps)
		for _ = 1, steps do
			at = nextOf[at]
		end
		return at
	end
	function work.computeShort(x, steps)
		for _ = 1, steps do
			x = (x * 1103515245 + 12345) % 2147483648
		end
		return x
	end

	-- (warm the code up, and the pages)
	local at = work.chaseLong(1, 20000)
	work.computeLong(at, 20000)

	-- calls of a hundred microseconds and more each: every one is read around
	Context.ProfilePhase("long calls")
	local x = 1
	for _ = 1, 150 do
		at = work.chaseLong(at, 2000)
		x = work.computeLong(x + at, 4000)
	end

	-- calls of 15 to 20 microseconds each: one in several is read around, and stands for the rest
	Context.ProfilePhase("short calls")
	for _ = 1, 2000 do
		at = work.chaseShort(at, 300)
		x = work.computeShort(x + at, 1000)
	end
	Context.ProfilePhase("done")
	print(at, x)
end
