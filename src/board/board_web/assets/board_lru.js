// One bounded recency group shared by the tab's draft maps: form drafts,
// form statuses, and editor drafts together hold at most 64 keys, and the
// least-recently-used key (use = read or write) evicts first. Each store is
// Map-like; recency and capacity are shared across all three.
export const DRAFT_STORE_CAPACITY = 64;

export function createDraftStores(capacity = DRAFT_STORE_CAPACITY) {
  const entries = new Map();
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
    return {
      get(key) {
        const at = slot(key);
        if (!entries.has(at)) return undefined;
        touch(at);
        return entries.get(at);
      },
      set(key, value) {
        const at = slot(key);
        if (entries.has(at)) entries.delete(at);
        entries.set(at, value);
        evict();
        return this;
      },
      delete(key) {
        return entries.delete(slot(key));
      },
      has(key) {
        return entries.has(slot(key));
      },
      clear() {
        for (const at of [...entries.keys()]) if (at.startsWith(prefix)) entries.delete(at);
      },
      get size() {
        let count = 0;
        for (const at of entries.keys()) if (at.startsWith(prefix)) count++;
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
