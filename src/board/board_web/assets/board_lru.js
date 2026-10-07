// One bounded recency group shared by the tab's draft maps: form drafts,
// form statuses, and editor drafts together hold at most 64 keys, and the
// least-recently-used key (use = read or write) evicts first. Each store is
// Map-like; recency and capacity are shared across all three. Editor input
// stores `editor:*` snapshots and the plan editor's `new` or `edit:*` draft
// outside the bound. Pins are LRU-capped: a pin past the cap demotes the
// oldest into the shared group. Successful saves delete their draft keys;
// opening an editor and showing a save confirmation create no entries.
export const DRAFT_STORE_CAPACITY = 64;
export const DRAFT_PIN_CAPACITY = 8;

export function createDraftStores(capacity = DRAFT_STORE_CAPACITY, pinCapacity = DRAFT_PIN_CAPACITY) {
  const entries = new Map();
  const pinned = new Map();
  const touch = (map, slot) => {
    const value = map.get(slot);
    map.delete(slot);
    map.set(slot, value);
  };
  const evict = () => {
    while (entries.size > capacity) entries.delete(entries.keys().next().value);
  };
  const demoteOldestPin = () => {
    const oldest = pinned.keys().next().value;
    entries.set(oldest, pinned.get(oldest));
    pinned.delete(oldest);
  };
  function store(prefix, pinnable = key => key.startsWith("editor:")) {
    const slot = key => `${prefix}${key}`;
    return {
      get(key) {
        const at = slot(key);
        if (pinned.has(at)) {
          touch(pinned, at);
          return pinned.get(at);
        }
        if (!entries.has(at)) return undefined;
        touch(entries, at);
        return entries.get(at);
      },
      set(key, value) {
        const at = slot(key);
        pinned.delete(at);
        entries.delete(at);
        if (pinnable(String(key))) {
          while (pinned.size >= pinCapacity && pinned.size) demoteOldestPin();
          pinned.set(at, value);
        } else {
          entries.set(at, value);
        }
        evict();
        return this;
      },
      delete(key) {
        const at = slot(key);
        return pinned.delete(at) || entries.delete(at);
      },
      has(key) {
        const at = slot(key);
        return pinned.has(at) || entries.has(at);
      },
      clear() {
        for (const map of [entries, pinned]) {
          for (const at of [...map.keys()]) if (at.startsWith(prefix)) map.delete(at);
        }
      },
      get size() {
        let count = 0;
        for (const map of [entries, pinned]) {
          for (const at of map.keys()) if (at.startsWith(prefix)) count++;
        }
        return count;
      },
    };
  }
  return {
    formDrafts: store("form\0"),
    formStatuses: store("status\0"),
    drafts: store("draft\0",
      key => key === "new" || key.startsWith("edit:") || key.startsWith("editor:")),
  };
}
