local SignalQueue = {}
local function normalizeSignal(signal: string): string
    local normalized = string.lower(signal)
    return normalized
end
function SignalQueue.enqueueSignal(signal: string)
    return normalizeSignal(signal)
end
return SignalQueue
