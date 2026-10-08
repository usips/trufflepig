// Browser globals for board asset tests under node:test: storage, Web Locks,
// same-thread broadcast, page visibility, and a DOM tree with selectors,
// focus gating, and a seedable body with id lookup.
import { createLocks, resetLocksShim } from "./locks_shim.mjs";

function createStorage() {
  const values = new Map();
  return {
    getItem: key => (values.has(String(key)) ? values.get(String(key)) : null),
    setItem: (key, value) => { values.set(String(key), String(value)); },
    removeItem: key => { values.delete(String(key)); },
    clear: () => values.clear(),
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
    const held = String(element.attributes.get("class") || element.className || "")
      .split(/\s+/).filter(Boolean);
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
  get classList() {
    return {
      toggle: (name, force) => {
        const names = new Set(this.className.split(/\s+/).filter(Boolean));
        const enabled = force ?? !names.has(name);
        if (enabled) names.add(name); else names.delete(name);
        this.className = [...names].join(" ");
        return enabled;
      },
    };
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
  get childElementCount() { return this.children.length; }
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

const documentListeners = new Map();
let root = null;
let body = null;
function findById(node, id) {
  for (const child of node.children) {
    if (child.id === id) return child;
    const found = findById(child, id);
    if (found) return found;
  }
  return null;
}
export function installDomShim() {
  if (root) return;
  root = new FakeElement("html");
  body = new FakeElement("body"); root.append(body);
  // Node's navigator binding is getter-only; redefine it with locks.
  Object.defineProperty(globalThis, "navigator", {
    value: { locks: createLocks() }, writable: true, configurable: true,
  });
  globalThis.sessionStorage = createStorage(); globalThis.localStorage = createStorage();
  globalThis.BroadcastChannel = ShimBroadcastChannel;
  globalThis.document = {
    activeElement: null,
    visibilityState: "visible",
    body,
    createElement: tag => new FakeElement(tag),
    createElementNS: (namespaceURI, tag) => Object.assign(new FakeElement(tag), { namespaceURI }),
    getElementById: id => findById(root, String(id)),
    querySelector: selector => root.querySelector(selector),
    querySelectorAll: selector => root.querySelectorAll(selector),
    addEventListener(type, listener) {
      (documentListeners.get(type) || documentListeners.set(type, []).get(type)).push(listener);
    },
    dispatchEvent(event) {
      for (const listener of documentListeners.get(event.type) || []) listener(event);
      return true;
    },
  };
  if (!globalThis.CSS) globalThis.CSS = { escape: value => String(value) };
}

export function resetDomShim() {
  sessionStorage.clear(); localStorage.clear();
  resetLocksShim(); channelPeers.clear(); documentListeners.clear();
  document.activeElement = null; document.visibilityState = "visible";
  root.replaceChildren(); root.append(body); body.replaceChildren();
}
