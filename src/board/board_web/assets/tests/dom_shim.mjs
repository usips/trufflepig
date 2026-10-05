// Browser globals for board asset tests under node:test: storage, strict Web
// Locks, same-thread broadcast, and a DOM tree with selectors and focus gating.
function createStorage() {
  const values = new Map();
  return {
    getItem: key => (values.has(String(key)) ? values.get(String(key)) : null),
    setItem: (key, value) => { values.set(String(key), String(value)); },
    removeItem: key => { values.delete(String(key)); },
    clear: () => values.clear(),
  };
}

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
function createLocks() {
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
      entry.waiters.push(waiter);
      signal?.addEventListener("abort", () => {
        const at = entry.waiters.indexOf(waiter);
        if (at > 0 || (at === 0 && !entry.busy)) { entry.waiters.splice(at, 1); waiter.reject(signal.reason); }
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
    if (!peers) channelPeers.set(this.name, peers = new Set());
    peers.add(this);
  }
  postMessage(data) {
    for (const peer of [...(channelPeers.get(this.name) || [])]) {
      if (peer !== this && typeof peer.onmessage === "function") queueMicrotask(() => peer.onmessage({ data }));
    }
  }
  close() { channelPeers.get(this.name)?.delete(this); }
}

function matchSelector(element, selector) {
  return selector.split(",").some(part => {
    const parsed = /^([a-zA-Z][\w-]*)?((?:\.[\w-]+)*)((?:\[[^\]]+\])*)$/.exec(part.trim());
    if (!parsed) return false;
    const [, tag, classes, conditions] = parsed;
    if (tag && element.tagName !== tag.toUpperCase()) return false;
    const held = String(element.className || "").split(/\s+/).filter(Boolean);
    if ((classes || "").split(".").filter(Boolean).some(cls => !held.includes(cls))) return false;
    return (conditions.match(/\[[^\]]+\]/g) || []).every(condition => {
      const body = condition.slice(1, -1);
      const equal = body.indexOf("=");
      const name = equal < 0 ? body : body.slice(0, equal);
      const key = name.slice(5).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
      const value = name.startsWith("data-") ? element.dataset[key] : element.attributes.get(name);
      if (equal < 0) return value !== undefined;
      return value !== undefined && String(value) === body.slice(equal + 1).replace(/^["']|["']$/g, "");
    });
  });
}

function isFocusable(element) {
  return element.attributes.has("tabindex")
    || (element.tagName === "A" && element.href !== undefined)
    || ["BUTTON", "INPUT", "SELECT", "TEXTAREA"].includes(element.tagName);
}
class FakeElement {
  constructor(tagName) {
    this.tagName = String(tagName).toUpperCase();
    this.children = []; this.parentElement = null; this.dataset = {}; this.attributes = new Map();
    this.className = ""; this.id = ""; this.textContent = ""; this.listeners = new Map();
  }
  append(...nodes) {
    for (const node of nodes.flat(Infinity)) {
      if (node !== null && node !== undefined && node !== false) { node.parentElement = this; this.children.push(node); }
    }
  }
  replaceChildren(...nodes) { this.children = []; this.append(...nodes); }
  addEventListener(type, listener) {
    (this.listeners.get(type) || this.listeners.set(type, []).get(type)).push(listener);
  }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  focus() { if (isFocusable(this)) globalThis.document.activeElement = this; }
  contains(node) { for (let at = node; at; at = at.parentElement) if (at === this) return true; return false; }
  closest(selector) { for (let at = this; at; at = at.parentElement) if (matchSelector(at, selector)) return at; return null; }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  querySelectorAll(selector) {
    const found = [];
    const walk = node => node.children.forEach(child => {
      if (matchSelector(child, selector)) found.push(child);
      walk(child);
    });
    walk(this); return found;
  }
}

let root = null;
export function installDomShim() {
  if (root) return;
  root = new FakeElement("html");
  // Node's navigator binding is getter-only; redefine it with locks.
  Object.defineProperty(globalThis, "navigator", {
    value: { locks: createLocks() }, writable: true, configurable: true,
  });
  globalThis.sessionStorage = createStorage(); globalThis.localStorage = createStorage();
  globalThis.BroadcastChannel = ShimBroadcastChannel;
  globalThis.document = {
    activeElement: null,
    createElement: tag => new FakeElement(tag),
    querySelector: selector => root.querySelector(selector),
    querySelectorAll: selector => root.querySelectorAll(selector),
  };
  if (!globalThis.CSS) globalThis.CSS = { escape: value => String(value) };
}

export function resetDomShim() {
  sessionStorage.clear(); localStorage.clear();
  lockQueues.clear(); channelPeers.clear();
  document.activeElement = null; root.replaceChildren();
}
