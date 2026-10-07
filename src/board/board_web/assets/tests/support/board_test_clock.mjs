// A deterministic clock: advancing time runs due callbacks in registration order,
// yielding to promise continuations after each callback without a wall-clock delay.
export function createBoardTestClock(start = 1700000000000) {
  let current = start;
  let nextId = 1;
  const timers = new Map();
  function schedule(callback, delay, interval, args) {
    const id = nextId++;
    const gap = Math.max(interval ? 1 : 0, Number(delay) || 0);
    timers.set(id, { callback, due: current + gap, gap, interval, args });
    return id;
  }
  const clear = id => timers.delete(id);
  return {
    now: () => current,
    setInterval: (callback, delay, ...args) => schedule(callback, delay, true, args),
    clearInterval: clear,
    setTimeout: (callback, delay, ...args) => schedule(callback, delay, false, args),
    clearTimeout: clear,
    pending: () => timers.size,
    clear: () => timers.clear(),
    async advance(milliseconds) {
      const until = current + milliseconds;
      while (true) {
        let next;
        for (const entry of timers) {
          if (entry[1].due <= until && (!next || entry[1].due < next[1].due)) next = entry;
        }
        if (!next) break;
        const [id, timer] = next;
        current = timer.due;
        if (timer.interval) timer.due += timer.gap;
        else timers.delete(id);
        timer.callback(...timer.args);
        await Promise.resolve();
      }
      current = until;
      await Promise.resolve();
    },
  };
}

export function createBoardTestSignal() {
  let release;
  const ready = new Promise(resolve => { release = resolve; });
  return { ready, release };
}
