-- EncodingService's buffers and errors (a playing client answers as a server does).
local Encoding = game:GetService("EncodingService")
local function show(Value)
	if typeof(Value) == "buffer" then
		return "buffer[" .. buffer.len(Value) .. "] " .. buffer.tostring(Value)
	end
	return typeof(Value) .. " " .. tostring(Value)
end
local function try(Label, Function)
	local Results = table.pack(pcall(Function))
	if not Results[1] then
		print(Label .. " error " .. P.err(Results[2]))
		return
	end
	local Parts = { Label, "ok", tostring(Results.n - 1) }
	for Index = 2, Results.n do
		table.insert(Parts, show(Results[Index]))
	end
	print(table.concat(Parts, " "))
end
try("b64enc", function()
	return Encoding:Base64Encode(buffer.fromstring("hello world"))
end)
try("b64enc-empty", function()
	return Encoding:Base64Encode(buffer.create(0))
end)
try("b64dec", function()
	return Encoding:Base64Decode(buffer.fromstring("aGVsbG8gd29ybGQ="))
end)
try("b64dec-unpadded", function()
	return Encoding:Base64Decode(buffer.fromstring("aGVsbG8gd29ybGQ"))
end)
try("b64dec-bad", function()
	return Encoding:Base64Decode(buffer.fromstring("@@@@"))
end)
try("b64enc-string", function()
	return Encoding:Base64Encode("hello")
end)
try("b64enc-nil", function()
	return Encoding:Base64Encode()
end)
local Data = buffer.fromstring(string.rep("abcdefgh", 64))
try("compress", function()
	local Compressed = Encoding:CompressBuffer(Data, Enum.CompressionAlgorithm.Zstd)
	return typeof(Compressed), buffer.tostring(Encoding:DecompressBuffer(Compressed, Enum.CompressionAlgorithm.Zstd)) == buffer.tostring(Data),
		Encoding:GetDecompressedBufferSize(Compressed, Enum.CompressionAlgorithm.Zstd)
end)
try("compress-level-22", function()
	return typeof(Encoding:CompressBuffer(Data, Enum.CompressionAlgorithm.Zstd, 22))
end)
try("compress-level-23", function()
	return Encoding:CompressBuffer(Data, Enum.CompressionAlgorithm.Zstd, 23)
end)
try("compress-no-algorithm", function()
	return Encoding:CompressBuffer(Data)
end)
try("decompress-bad", function()
	return Encoding:DecompressBuffer(buffer.fromstring("not zstd"), Enum.CompressionAlgorithm.Zstd)
end)
