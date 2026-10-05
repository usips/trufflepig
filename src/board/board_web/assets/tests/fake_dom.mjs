// Minimal document/element double for board DOM unit tests: dataset,
// tree, tag/.class/#id/[data-*] selectors, closest, and focus tracking.
function splitAlternatives(selector) {
  const parts = [];
  let depth = 0, quote = "", current = "";
  for (const character of selector) {
    if (quote) {
      current += character;
      if (character === quote) quote = "";
    } else if (character === '"' || character === "'") {
      quote = character;
      current += character;
    } else if (character === "[") {
      depth++;
      current += character;
    } else if (character === "]") {
      depth--;
      current += character;
    } else if (character === "," && depth === 0) {
      parts.push(current.trim());
      current = "";
    } else {
      current += character;
    }
  }
  if (current.trim()) parts.push(current.trim());
  return parts;
}

function readAttribute(element, name) {
  if (name.startsWith("data-")) {
    const key = name.slice(5).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
    return element.dataset[key];
  }
  const direct = element[name];
  if (typeof direct === "string") return direct;
  return element.attributes.get(name);
}

function matchAlternative(element, alternative) {
  const pattern = /^(?:([a-zA-Z][\w-]*)|\*)?((?:\.[\w-]+)*)?(?:#([\w-]+))?((?:\[[^\]]+\])*)$/;
  const parsed = pattern.exec(alternative.trim());
  if (!parsed) return false;
  const [, tag, classes, id, conditions] = parsed;
  if (tag && element.tagName !== tag.toUpperCase()) return false;
  if (id && element.id !== id) return false;
  const held = String(element.className || "").split(/\s+/).filter(Boolean);
  for (const cls of (classes || "").split(".").filter(Boolean)) {
    if (!held.includes(cls)) return false;
  }
  for (const condition of conditions.match(/\[[^\]]+\]/g) || []) {
    const body = condition.slice(1, -1);
    const operator = body.match(/([\^$*]?=)/);
    if (!operator) {
      if (readAttribute(element, body) === undefined) return false;
      continue;
    }
    const name = body.slice(0, operator.index);
    let want = body.slice(operator.index + operator[1].length);
    if ((want.startsWith('"') && want.endsWith('"')) || (want.startsWith("'") && want.endsWith("'"))) {
      want = want.slice(1, -1);
    }
    want = want.replace(/\\(.)/g, "$1");
    const value = readAttribute(element, name);
    if (value === undefined || value === null) return false;
    if (operator[1] === "=" && String(value) !== want) return false;
    if (operator[1] === "^=" && !String(value).startsWith(want)) return false;
  }
  return true;
}

function matchSelector(element, selector) {
  return splitAlternatives(selector).some(alternative => matchAlternative(element, alternative));
}

export class FakeElement {
  constructor(tagName) {
    this.tagName = String(tagName).toUpperCase();
    this.children = [];
    this.parentElement = null;
    this.dataset = {};
    this.attributes = new Map();
    this.className = "";
    this.id = "";
    this.textContent = "";
    this.listeners = new Map();
  }
  append(...nodes) {
    for (const node of nodes.flat(Infinity)) {
      if (node === null || node === undefined || node === false) continue;
      node.parentElement = this;
      this.children.push(node);
    }
  }
  appendChild(node) {
    this.append(node);
    return node;
  }
  replaceChildren(...nodes) {
    this.children = [];
    this.append(...nodes);
  }
  addEventListener(type, listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(listener);
  }
  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }
  removeAttribute(name) {
    this.attributes.delete(name);
  }
  focus() {
    if (globalThis.document) globalThis.document.activeElement = this;
  }
  contains(node) {
    let at = node;
    while (at) {
      if (at === this) return true;
      at = at.parentElement;
    }
    return false;
  }
  closest(selector) {
    let at = this;
    while (at) {
      if (matchSelector(at, selector)) return at;
      at = at.parentElement;
    }
    return null;
  }
  querySelector(selector) {
    return this.querySelectorAll(selector)[0] || null;
  }
  querySelectorAll(selector) {
    const found = [];
    const walk = node => {
      for (const child of node.children) {
        if (matchSelector(child, selector)) found.push(child);
        walk(child);
      }
    };
    walk(this);
    return found;
  }
}

export function installFakeDom() {
  const root = new FakeElement("html");
  globalThis.document = {
    activeElement: null,
    root,
    createElement: tag => new FakeElement(tag),
    querySelector: selector => root.querySelector(selector),
    querySelectorAll: selector => root.querySelectorAll(selector),
    getElementById: id => root.querySelector(`#${id}`),
  };
  if (!globalThis.CSS) globalThis.CSS = { escape: value => String(value) };
}

export function resetFakeDom() {
  globalThis.document.activeElement = null;
  globalThis.document.root.children = [];
}
