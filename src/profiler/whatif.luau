-- The harness `ree profile try` runs (src/profiler/whatif.rs, docs/cli.md "What-if comparisons"). It mounts the variant file,
-- then, by Context.Mode: lists its variants (list), profiles one variant at one size (profile), times one (time), or runs
-- every variant at every size and checks their results against the baseline's (check). The runner writes this file into
-- the comparison's folder on every run.
local HttpService = game:GetService("HttpService")

local function fail(message)
	error("ree profile try: " .. message, 0)
end

local function say(kind, fields)
	print("@try-" .. kind .. " " .. HttpService:JSONEncode(fields))
end

-- A value's contents as text, keys sorted, for a digest (equal results read the same)
local function canonical(value, seen, out)
	local kind = typeof(value)
	if kind == "table" then
		if seen[value] then
			table.insert(out, "<cycle>")
			return
		end
		seen[value] = true
		local keys = {}
		for key in next, value do
			table.insert(keys, key)
		end
		table.sort(keys, function(a, b)
			local ta, tb = typeof(a), typeof(b)
			if ta ~= tb then
				return ta < tb
			end
			if ta == "number" or ta == "string" then
				return a < b
			end
			return tostring(a) < tostring(b)
		end)
		table.insert(out, "{")
		for _, key in keys do
			canonical(key, seen, out)
			table.insert(out, "=")
			canonical(value[key], seen, out)
			table.insert(out, ",")
		end
		table.insert(out, "}")
		seen[value] = nil
	elseif kind == "number" then
		table.insert(out, string.format("%.17g", value))
	elseif kind == "string" then
		table.insert(out, string.format("%q", value))
	elseif kind == "function" or kind == "thread" then
		table.insert(out, kind)
	else
		table.insert(out, kind .. ":" .. tostring(value))
	end
end

local function digest(value)
	local out = {}
	canonical(value, {}, out)
	local text = table.concat(out)
	local hash = 5381
	for index = 1, #text, 64 do
		for _, byte in { string.byte(text, index, math.min(index + 63, #text)) } do
			hash = (hash * 33 + byte) % 4294967296
		end
	end
	return string.format("%08x", hash)
end

local function deepEqual(a, b, seen)
	if a == b then
		return true
	end
	if type(a) ~= "table" or type(b) ~= "table" then
		return false
	end
	seen = seen or {}
	if seen[a] == b then
		return true
	end
	seen[a] = b
	for key, value in next, a do
		if not deepEqual(value, b[key], seen) then
			return false
		end
	end
	for key in next, b do
		if a[key] == nil then
			return false
		end
	end
	return true
end

-- The instance at a path ("ReplicatedStorage/Shared/Handler", or with dots when no slash)
local function find(path)
	local separator = if string.find(path, "/", 1, true) then "/" else "."
	local parts = string.split(path, separator)
	local ok, current = pcall(game.GetService, game, parts[1])
	if not ok or current == nil then
		fail(`module {path}: {parts[1]} is not a service`)
	end
	for index = 2, #parts do
		local child = current:FindFirstChild(parts[index])
		if child == nil then
			fail(`module {path}: {current:GetFullName()} has no {parts[index]}`)
		end
		current = child
	end
	if not current:IsA("ModuleScript") then
		fail(`module {path} is a {current.ClassName}, not a ModuleScript`)
	end
	return current
end

-- The variant file, read: { title, about, sizes, frames, check, variants = { { name, kind, setup, run, source, file } } }
local function load(Context)
	local stem = string.match(Context.File, "([^/\\]+)$") or "Variants"
	stem = string.gsub(stem, "%.luau?$", "")
	local Spec = require(Context.Mount("ReplicatedStorage/WhatIf/" .. stem, Context.File))
	if type(Spec) ~= "table" then
		fail("the variant file returns a table: { sizes = ..., setup = ..., variants = ... }")
	end
	local spec = {
		title = Spec.title,
		about = Spec.about,
		sizes = Spec.sizes or { 1 },
		frames = Spec.frames or 1,
		check = Spec.check,
		module = Spec.module,
		variants = {},
	}
	if type(spec.sizes) ~= "table" or #spec.sizes == 0 then
		fail("sizes is a list of numbers: { 10, 100, 1000 }")
	end
	for _, size in spec.sizes do
		if type(size) ~= "number" or size < 0 then
			fail("sizes is a list of numbers at or above 0")
		end
	end
	if type(spec.frames) ~= "number" or spec.frames < 1 or spec.frames % 1 ~= 0 then
		fail("frames is a whole number from 1")
	end
	if spec.check ~= nil and spec.check ~= true and type(spec.check) ~= "function" then
		fail("check is a function(a, b, n) or true")
	end
	local sharedSetup = Spec.setup
	if sharedSetup ~= nil and type(sharedSetup) ~= "function" then
		fail("setup is a function(n)")
	end
	local function add(name, entry, kind, file)
		if type(name) ~= "string" or name == "" then
			fail("a variant's name is a string")
		end
		local setup, run
		if type(entry) == "function" then
			setup, run = sharedSetup, entry
		elseif type(entry) == "table" then
			setup, run = entry.setup or sharedSetup, entry.run
		end
		if type(run) ~= "function" then
			fail(`variant {name}: a function(data), or {"{"} setup = function(n), run = function(data) {"}"}`)
		end
		local source, line = debug.info(run, "sl")
		table.insert(spec.variants, {
			name = name,
			kind = kind,
			setup = setup or function(n)
				return n
			end,
			run = run,
			source = if kind == "module" then file else `{source}:{line}`,
			file = file,
		})
	end
	if Spec.versions ~= nil then
		if type(Spec.versions) ~= "table" then
			fail("versions is { name = \"file.luau\" } or a list of { name = ..., file = ... }")
		end
		if Spec.variants ~= nil then
			fail("a variant file has variants or versions, not both")
		end
		local driver = { setup = Spec.setup, run = Spec.run }
		if type(driver.run) ~= "function" then
			fail("versions need run = function(data, Module) (and setup = function(n, Module))")
		end
		local list = {}
		if #Spec.versions > 0 then
			for _, version in Spec.versions do
				table.insert(list, { version.name, version.file })
			end
		else
			for name, file in Spec.versions do
				table.insert(list, { name, file })
			end
			table.sort(list, function(a, b)
				return a[1] < b[1]
			end)
		end
		for _, version in list do
			if type(version[2]) ~= "string" then
				fail(`version {version[1]}: a file path`)
			end
			add(version[1], driver, "module", version[2])
		end
	elseif type(Spec.variants) == "table" then
		if #Spec.variants > 0 then
			for _, variant in Spec.variants do
				if type(variant) ~= "table" then
					fail("a list of variants holds { name = ..., run = ..., setup = ... }")
				end
				add(variant.name, variant, "function")
			end
		else
			local names = {}
			for name in Spec.variants do
				table.insert(names, name)
			end
			table.sort(names)
			for _, name in names do
				add(name, Spec.variants[name], "function")
			end
		end
	else
		fail("the variant file has variants = { name = function(data) ... } or versions = { name = \"file.luau\" }")
	end
	if #spec.variants == 0 then
		fail("no variants")
	end
	return spec
end

-- A version's module (Context.Sources holds the versions' sources, read by the runner): standing in for the project's
-- module when the file names one, else mounted on its own. `fresh` makes a copy beside it (the check runs every version in
-- one engine).
local function versionModule(Context, spec, variant, fresh)
	local source = Context.Sources and Context.Sources[variant.name]
	if type(source) ~= "string" then
		fail(`version {variant.name}: its source was not passed`)
	end
	if spec.module == nil then
		return require(Context.Mount("ReplicatedStorage/WhatIf/Versions/" .. variant.name, Context.Paths[variant.name]))
	end
	local target = find(spec.module)
	if fresh then
		local copy = target:Clone()
		Context.StandIn(copy, source)
		copy.Parent = target.Parent
		return require(copy)
	end
	Context.StandIn(target, source)
	return require(target)
end

local function pick(spec, name)
	for _, variant in spec.variants do
		if variant.name == name then
			return variant
		end
	end
	fail(`no variant {name}`)
end

local function trace(message)
	return debug.traceback(tostring(message), 2)
end

local Modes = {}

function Modes.list(Context)
	local spec = load(Context)
	local variants = {}
	for _, variant in spec.variants do
		table.insert(variants, { name = variant.name, kind = variant.kind, source = variant.source, file = variant.file })
	end
	say("list", {
		title = spec.title,
		about = spec.about,
		sizes = spec.sizes,
		frames = spec.frames,
		check = spec.check ~= nil,
		module = spec.module,
		variants = variants,
	})
end

function Modes.profile(Context)
	local spec = load(Context)
	local variant = pick(spec, Context.Variant)
	local frames = Context.Frames or spec.frames
	local Module = if variant.kind == "module" then versionModule(Context, spec, variant, false) else nil
	-- setup in a frame of its own, then the run from a frame after it (what it touches of setup's data is cold: made before
	-- the frame), one call a frame
	task.wait()
	Context.ProfilePhase("setup")
	local ok, data = xpcall(variant.setup, trace, Context.Size, Module)
	if not ok then
		say("error", { phase = "setup", message = data })
		return
	end
	task.wait()
	task.wait()
	Context.ProfilePhase("run")
	local result
	for frame = 1, frames do
		local done, value = xpcall(variant.run, trace, data, Module)
		if not done then
			say("error", { phase = "run", message = value })
			return
		end
		result = value
		if frame < frames then
			task.wait()
		end
	end
	-- (the result still held: the snapshot at this mark sees it)
	Context.ProfilePhase("after")
	say("result", { digest = digest(result) })
end

function Modes.time(Context)
	local spec = load(Context)
	local variant = pick(spec, Context.Variant)
	local frames = Context.Frames or spec.frames
	local reps = Context.Reps or 1
	local group = math.clamp(Context.Group or reps, 1, reps)
	local Module = if variant.kind == "module" then versionModule(Context, spec, variant, false) else nil
	local run = variant.run
	task.wait()
	task.wait()
	-- inputs in groups that fit the CPU's caches, each group's calls back to back between two marks: the runner adds up
	-- the host time of the "run" stretches
	local done = 0
	while done < reps do
		local count = math.min(group, reps - done)
		local inputs = table.create(count)
		for index = 1, count do
			inputs[index] = variant.setup(Context.Size, Module)
		end
		Context.ProfilePhase("run")
		for index = 1, count do
			local data = inputs[index]
			for _ = 1, frames do
				run(data, Module)
			end
		end
		Context.ProfilePhase("rest")
		done += count
	end
	Context.ProfilePhase("after")
end

function Modes.check(Context)
	local spec = load(Context)
	local frames = Context.Frames or spec.frames
	local check = spec.check
	if check == true then
		check = function(a, b)
			return deepEqual(a, b), "the results differ"
		end
	end
	local wanted = Context.Variants
	local variants = {}
	if wanted then
		for _, name in wanted do
			table.insert(variants, pick(spec, name))
		end
	else
		variants = spec.variants
	end
	local modules = {}
	for _, variant in variants do
		if variant.kind == "module" then
			modules[variant.name] = versionModule(Context, spec, variant, true)
		end
	end
	for _, size in Context.Sizes or spec.sizes do
		local baseline
		for index, variant in variants do
			local Module = modules[variant.name]
			local ok, result = xpcall(function()
				local data = variant.setup(size, Module)
				local value
				for _ = 1, frames do
					value = variant.run(data, Module)
				end
				return value
			end, trace)
			if not ok then
				say("check", { variant = variant.name, size = size, ok = false, message = `raised: {result}` })
				if index == 1 then
					break
				end
				continue
			end
			if index == 1 then
				baseline = result
				say("check", { variant = variant.name, size = size, ok = true, digest = digest(result) })
				continue
			end
			local same, message = true, nil
			if check then
				local checked, verdict, why = pcall(check, baseline, result, size)
				if not checked then
					same, message = false, `check raised: {verdict}`
				elseif verdict ~= true then
					same, message = false, if why ~= nil then tostring(why) else "check returned " .. tostring(verdict)
				end
			end
			say("check", { variant = variant.name, size = size, ok = same, message = message, digest = digest(result) })
		end
	end
end

return function(Context)
	local mode = Modes[Context.Mode]
	if mode == nil then
		fail(`no mode {Context.Mode}`)
	end
	local ok, message = xpcall(mode, trace, Context)
	if not ok then
		say("fail", { message = message })
	end
	print("@done")
end
