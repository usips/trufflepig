// Per-database seen mark: the highest event sequence this browser has had
// delivered, namespaced by the board id so port-0 restarts never mix marks.
// Every write re-reads storage and keeps the max, so a lagging tab cannot
// lower a mark another tab already raised. Only a fresh server snapshot may
// lower the mark, when the database was replaced underneath the tab.
export function seenKey(boardId) {
  return `trufflepig-board-seen:${boardId || ""}`;
}

export function readSeenMark(storage, key) {
  try {
    const raw = storage?.getItem(key);
    return raw && /^(0|[1-9]\d*)$/.test(raw) ? raw : null;
  } catch (_) {
    return null;
  }
}

export function writeSeenMark(storage, key, seq) {
  const text = String(seq);
  if (!/^(0|[1-9]\d*)$/.test(text)) return readSeenMark(storage, key);
  const stored = readSeenMark(storage, key);
  const mark = stored !== null && BigInt(stored) > BigInt(text) ? stored : text;
  if (mark !== stored) {
    try {
      storage?.setItem(key, mark);
    } catch (_) { /* The mark is a convenience, not a cursor. */ }
  }
  return mark;
}

export function resyncSeenMark(storage, key, serverWatermark) {
  const text = String(serverWatermark);
  if (!/^(0|[1-9]\d*)$/.test(text)) return readSeenMark(storage, key);
  const stored = readSeenMark(storage, key);
  if (stored === null || BigInt(stored) <= BigInt(text)) return stored;
  try {
    storage?.setItem(key, text);
  } catch (_) { /* The mark is a convenience, not a cursor. */ }
  return text;
}
