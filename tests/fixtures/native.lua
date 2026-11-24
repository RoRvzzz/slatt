local tally = 0
local function update(x)
    tally = tally + x
    return tally
end

local state = {a = 1, b = 2}
for _, value in pairs(state) do update(value) end
for i = 1, 3 do update(i) end

local value = math.abs(-10 + tally)
local ok, result = pcall(update, value)
tally = 17
return update, ok, result, "\255ok"
