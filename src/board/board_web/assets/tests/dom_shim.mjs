// Minimal browser globals for board asset tests under node:test: storage,
// Web Locks, same-thread broadcast, a fetch stub, and meta reading.
const storages = new Set();
function createStorage() {
  const values = new Map();
  const storage = {
    getItem: key => (values.has(String(key)) ? values.get(String(key)) : null),
    setItem: (key, value) => { values.set(String(key), String(value)); },
    removeItem: key => { values.delete(String(key)); },
    clear: () => values.clear(),
    get length() { return values.size; },
  };
  storages.add(storage);
  return storage;
}

const lockQueues = new Map();
function pumpLock(name) {
  const entry = lockQueues.get(name);
  if (!entry || entry.busy || entry.waiters.length === 0) return;
  const head = entry.waiters[0];
  entry.busy = true;
  head.granted = true;
  queueMicrotask(async () => {
    try {
      head.resolve(await head.run());
    } catch (error) {
      head.reject(error);
    } finally {
      if (head.evicted) return;
      entry.waiters.shift();
      entry.busy = false;
      pumpLock(name);
    }
  });
}
function createLocks() {
  return {
    request(name, options, callback) {
      const run = typeof options === "function" ? options : callback;
      const opts = typeof options === "function" ? {} : options || {};
      const signal = opts.signal;
      if (signal?.aborted) return Promise.reject(signal.reason);
      const waiter = { run, signal, granted: false, resolve: null, reject: null };
      const pending = new Promise((resolve, reject) => {
        waiter.resolve = resolve;
        waiter.reject = reject;
      });
      let entry = lockQueues.get(name);
      if (!entry) {
        entry = { waiters: [], busy: false };
        lockQueues.set(name, entry);
      }
      // A steal preempts the held lock like the real API: the evicted holder
      // keeps running, and its late release no longer touches the queue.
      if (opts.steal && entry.busy && entry.waiters.length > 0) {
        entry.waiters[0].evicted = true;
        entry.waiters.shift();
        entry.busy = false;
      }
      entry.waiters.push(waiter);
      signal?.addEventListener("abort", () => {
        if (waiter.granted) return;
        const at = entry.waiters.indexOf(waiter);
        if (at >= 0) {
          entry.waiters.splice(at, 1);
          waiter.reject(signal.reason);
        }
      }, { once: true });
      pumpLock(name);
      return pending;
    },
  };
}

const channelPeers = new Map();
class ShimBroadcastChannel {
  constructor(name) {
    this.name = String(name);
    this.onmessage = null;
    let peers = channelPeers.get(this.name);
    if (!peers) {
      peers = new Set();
      channelPeers.set(this.name, peers);
    }
    peers.add(this);
  }
  postMessage(data) {
    const peers = channelPeers.get(this.name);
    if (!peers) return;
    for (const peer of [...peers]) {
      if (peer === this || typeof peer.onmessage !== "function") continue;
      queueMicrotask(() => peer.onmessage({ data }));
    }
  }
  close() {
    channelPeers.get(this.name)?.delete(this);
  }
}

let fetchStub = null;
export function setFetchStub(stub) { fetchStub = stub; }

let metaBoardApi = "1";
export function setMetaBoardApi(value) { metaBoardApi = value; }

let installed = false;
export function installDomShim() {
  if (installed) return;
  installed = true;
  // Node's navigator binding is getter-only; redefine it with locks.
  Object.defineProperty(globalThis, "navigator", {
    value: { locks: createLocks() }, writable: true, configurable: true,
  });
  globalThis.sessionStorage = createStorage();
  globalThis.localStorage = createStorage();
  globalThis.BroadcastChannel = ShimBroadcastChannel;
  globalThis.fetch = (...args) => (fetchStub
    ? fetchStub(...args)
    : Promise.reject(new Error("fetch is not stubbed")));
  globalThis.document = {
    querySelector: selector => (selector === 'meta[name="board-api"]' && metaBoardApi !== null
      ? { content: metaBoardApi }
      : null),
    getElementById: () => null,
  };
}

export function resetDomShim() {
  for (const storage of storages) storage.clear();
  lockQueues.clear();
  channelPeers.clear();
  fetchStub = null;
  metaBoardApi = "1";
}
