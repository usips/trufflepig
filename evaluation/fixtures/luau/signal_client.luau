local Queue = require("./signal_queue")
local function sendQueuedSignal(signal: string)
    return Queue.enqueueSignal(signal)
end
return sendQueuedSignal
