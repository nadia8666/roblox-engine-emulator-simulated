-- An array of records or a record of arrays: n moving things with a position, a velocity and health.
-- Run: one frame's update: move every one and count the living.
-- ree profile try tools/what-if/records-or-arrays.luau

local DT = 1 / 60

return {
	title = "array of records or record of arrays",
	about = "n things with x, y, vx, vy and hp: { {x=, y=, ...}, ... } or { x = {...}, y = {...}, ... }; run moves them all once",
	sizes = { 10, 100, 1000, 10000 },
	variants = {
		{
			name = "array of records",
			setup = function(n)
				local things = table.create(n)
				for i = 1, n do
					things[i] = { x = i, y = -i, vx = i % 7, vy = i % 5, hp = i % 3 }
				end
				return things
			end,
			run = function(things)
				local alive, sum = 0, 0
				for _, thing in things do
					thing.x += thing.vx * DT
					thing.y += thing.vy * DT
					if thing.hp > 0 then
						alive += 1
					end
					sum += thing.x
				end
				return { alive = alive, sum = math.floor(sum * 1000) }
			end,
		},
		{
			name = "record of arrays",
			setup = function(n)
				local x, y, vx, vy, hp = table.create(n), table.create(n), table.create(n), table.create(n), table.create(n)
				for i = 1, n do
					x[i], y[i], vx[i], vy[i], hp[i] = i, -i, i % 7, i % 5, i % 3
				end
				return { n = n, x = x, y = y, vx = vx, vy = vy, hp = hp }
			end,
			run = function(things)
				local x, y, vx, vy, hp = things.x, things.y, things.vx, things.vy, things.hp
				local alive, sum = 0, 0
				for i = 1, things.n do
					x[i] += vx[i] * DT
					y[i] += vy[i] * DT
					if hp[i] > 0 then
						alive += 1
					end
					sum += x[i]
				end
				return { alive = alive, sum = math.floor(sum * 1000) }
			end,
		},
	},
	check = true,
}
