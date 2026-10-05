// One bounded recency group shared by the tab's draft maps: form drafts,
// form statuses, and editor drafts together hold at most 64 keys, and the
// least-recently-used key (use = read or write) evicts first. Each store is
// Map-like; recency and capacity are shared across all three. Keys starting
// with `editor:` are pinned outside the bound, so a Tasks tab's snapshots
// can never evict an unsaved plan-editor draft.
export const DRAFT_STORE_CAPACITY = 64;

export function createDraftStores(capacity = DRAFT_STORE_CAPACITY) {
  const entries = new Map();
  const pinned = new Map();
  const touch = slot => {
    const value = entries.get(slot);
    entries.delete(slot);
    entries.set(slot, value);
  };
  const evict = () => {
    while (entries.size > capacity) entries.delete(entries.keys().next().value);
  };
  function store(prefix) {
    const slot = key => `${prefix}${key}`;
    const held = key => (String(key).startsWith("editor:") ? pinned : entries);
    return {
      get(key) {
        const map = held(key);
        const at = slot(key);
        if (!map.has(at)) return undefined;
        if (map === entries) touch(at);
        return map.get(at);
      },
      set(key, value) {
        const map = held(key);
        const at = slot(key);
        if (map.has(at)) map.delete(at);
        map.set(at, value);
        if (map === entries) evict();
        return this;
      },
      delete(key) {
        return held(key).delete(slot(key));
      },
      has(key) {
        return held(key).has(slot(key));
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
    drafts: store("draft\0"),
  };
}
