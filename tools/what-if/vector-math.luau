-- Vector3 math or plain numbers: moving n particles one step.
-- Run: position += velocity * dt for every particle, with Vector3 values or with three numbers each.
-- ree profile try tools/what-if/vector-math.luau

local DT = 1 / 60

return {
	title = "Vector3 math or plain numbers",
	about = "n particles stepped once: Vector3 positions and velocities, or x, y and z as numbers in arrays",
	sizes = { 10, 100, 1000, 10000 },
	variants = {
		{
			name = "Vector3",
			setup = function(n)
				local positions, velocities = table.create(n), table.create(n)
				for i = 1, n do
					positions[i] = Vector3.new(i, 0, -i)
					velocities[i] = Vector3.new(i % 7, 1, i % 5)
				end
				return { positions = positions, velocities = velocities }
			end,
			run = function(data)
				local positions, velocities = data.positions, data.velocities
				for i, position in positions do
					positions[i] = position + velocities[i] * DT
				end
				return positions
			end,
		},
		{
			name = "numbers",
			setup = function(n)
				local data = {}
				for _, key in { "x", "y", "z", "vx", "vy", "vz" } do
					data[key] = table.create(n)
				end
				for i = 1, n do
					data.x[i], data.y[i], data.z[i] = i, 0, -i
					data.vx[i], data.vy[i], data.vz[i] = i % 7, 1, i % 5
				end
				data.n = n
				return data
			end,
			run = function(data)
				local x, y, z, vx, vy, vz = data.x, data.y, data.z, data.vx, data.vy, data.vz
				for i = 1, data.n do
					x[i] += vx[i] * DT
					y[i] += vy[i] * DT
					z[i] += vz[i] * DT
				end
				return data
			end,
		},
	},
	-- the same positions: a Vector3 holds 32-bit floats, a number 64 bits, so they agree to a 32-bit float's precision
	check = function(vectors, numbers)
		for i, position in vectors do
			for _, axis in { "X", "Y", "Z" } do
				local number = numbers[string.lower(axis)][i]
				if math.abs(position[axis] - number) > math.max(1e-4, math.abs(number) * 2 ^ -22) then
					return false, `particle {i} {axis}: {position[axis]} and {number}`
				end
			end
		end
		return true
	end,
}
