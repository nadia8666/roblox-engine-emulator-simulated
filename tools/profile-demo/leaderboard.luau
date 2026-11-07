-- A leaderboard redrawn every frame, the slow way on purpose: something to profile.
-- Each frame copies every score into a new table, sorts them all and rewrites every row.
-- README.md "Profiling" runs it and opens the profile.
return function(Context)
	local RunService = game:GetService("RunService")

	local Scores = {}
	for Id = 1, 500 do
		Scores[Id] = { Name = `Player{Id}`, Points = (Id * 7919) % 10000 }
	end

	local Board = Instance.new("Folder")
	Board.Name = "Leaderboard"
	Board.Parent = workspace
	local Rows = {}
	for Rank = 1, 10 do
		local Row = Instance.new("StringValue")
		Row.Name = `Row{Rank}`
		Row.Parent = Board
		Rows[Rank] = Row
	end

	RunService.Heartbeat:Connect(function()
		for Id, Entry in Scores do
			Entry.Points += Id % 3
		end
		local Ranked = {}
		for _, Entry in Scores do
			table.insert(Ranked, { Name = Entry.Name, Points = Entry.Points })
		end
		table.sort(Ranked, function(A, B)
			return A.Points > B.Points
		end)
		for Rank, Row in Rows do
			local Entry = Ranked[Rank]
			Row.Value = `{Rank}. {Entry.Name}  {Entry.Points}`
		end
	end)

	task.wait(Context.Seconds)
end
