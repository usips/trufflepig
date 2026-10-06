// Strict Web Locks shim for board asset tests: per-name FIFO queues,
// abortable waiters, and steals that evict the holder and queue ahead of
// every waiting request.
const lockQueues = new Map();

function pumpLock(name) {
  const entry = lockQueues.get(name);
  if (!entry || entry.busy || !entry.waiters.length) return;
  const head = entry.waiters[0];
  entry.busy = true;
  queueMicrotask(async () => {
    try { head.resolve(await head.run()); }
    catch (error) { head.reject(error); }
    finally { if (!head.evicted) { entry.waiters.shift(); entry.busy = false; pumpLock(name); } }
  });
}

export function createLocks() {
  return {
    request(name, options, callback) {
      const run = typeof options === "function" ? options : callback;
      const opts = typeof options === "function" ? {} : options || {};
      const signal = opts.signal;
      if (signal && opts.steal) { // The spec makes the two mutually exclusive.
        return Promise.reject(new DOMException("signal with steal", "NotSupportedError"));
      }
      if (signal?.aborted) return Promise.reject(signal.reason);
      const waiter = { run, signal, evicted: false };
      const pending = new Promise((yes, no) => { waiter.resolve = yes; waiter.reject = no; });
      const entry = lockQueues.get(name) || lockQueues.set(name, { waiters: [], busy: false }).get(name);
      // A steal evicts the holder with AbortError; its late release leaves the queue alone.
      if (opts.steal && entry.busy && entry.waiters.length) {
        const held = entry.waiters.shift();
        held.evicted = true; entry.busy = false;
        held.reject(new DOMException("The lock was stolen", "AbortError"));
      }
      // The spec queues a stealing request ahead of every waiting request.
      if (opts.steal) entry.waiters.unshift(waiter);
      else entry.waiters.push(waiter);
      signal?.addEventListener("abort", () => {
        const at = entry.waiters.indexOf(waiter);
        if (at > 0 || (at === 0 && !entry.busy)) { entry.waiters.splice(at, 1); waiter.reject(signal.reason); }
      }, { once: true });
      pumpLock(name);
      return pending;
    },
  };
}

export function resetLocksShim() {
  lockQueues.clear();
}
