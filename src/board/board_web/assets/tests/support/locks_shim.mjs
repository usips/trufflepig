// Strict Web Locks shim for board asset tests: per-name FIFO queues,
// abortable waiters, and steals that evict the holder and queue ahead of
// every waiting request.
const lockQueues = new Map();
// Test knob: while armed, steal requests reject instead of queueing, like a
// browser refusing a steal; rejectedStealCount records how many were refused.
let stealsRejected = false;
let rejectedStealCount = 0;
// One-shot eviction between grant and callback dispatch; the rejected request's
// callback still runs after its rejection handler, as a delayed browser task can.
let abortBeforeCallback = false;
let abortedGrantCallbackCount = 0;

function pumpLock(name) {
  const entry = lockQueues.get(name);
  if (!entry || entry.busy || !entry.waiters.length) return;
  const head = entry.waiters[0];
  entry.busy = true;
  queueMicrotask(async () => {
    try {
      const abortGrant = abortBeforeCallback && !head.evicted;
      if (abortGrant) {
        abortBeforeCallback = false;
        head.evicted = true; entry.waiters.shift(); entry.busy = false;
        head.reject(new DOMException("The granted lock was stolen before callback", "AbortError"));
        pumpLock(name);
        await Promise.resolve();
      }
      const held = head.run();
      if (abortGrant) abortedGrantCallbackCount += 1;
      head.resolve(await held);
    }
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
      if (opts.steal && stealsRejected) {
        rejectedStealCount += 1;
        return Promise.reject(new DOMException("The steal was rejected", "NotSupportedError"));
      }
      const waiter = { run, signal, evicted: false };
      const pending = new Promise((yes, no) => { waiter.resolve = yes; waiter.reject = no; });
      const entry = lockQueues.get(name)
        || lockQueues.set(name, { waiters: [], busy: false }).get(name);
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
        if (at > 0 || (at === 0 && !entry.busy)) {
          entry.waiters.splice(at, 1); waiter.reject(signal.reason);
        }
      }, { once: true });
      pumpLock(name);
      return pending;
    },
  };
}

export function rejectSteals() { stealsRejected = true; }
export function allowSteals() { stealsRejected = false; }
export function stealRejections() { return rejectedStealCount; }
export function abortNextGrantBeforeCallback() { abortBeforeCallback = true; }
export function abortedGrantCallbacks() { return abortedGrantCallbackCount; }

export function resetLocksShim() {
  lockQueues.clear();
  stealsRejected = false;
  rejectedStealCount = 0;
  abortBeforeCallback = false;
  abortedGrantCallbackCount = 0;
}
