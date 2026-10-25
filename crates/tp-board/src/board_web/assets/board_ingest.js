// Pure ingest-receipt decision for the board tab: a tab completes only on the
// ticket its own 202 returned. Foreign tickets (other tabs' flights, replays
// from before this tab queued) are ignored, and a pending ticket still
// unconfirmed after thirty seconds resolves to unknown, never completed.
export const INGEST_TIMEOUT_MS = 30000;
export const INGEST_UNKNOWN_MESSAGE = "ingest result unknown";
// Receipts publish the moment a scan ends, possibly before the tab's 202
// arrives; the tab keeps the last eight receipts by ticket so a 202 can
// complete from a receipt that landed early.
export const INGEST_RECEIPT_CACHE_LIMIT = 8;

export function resolveIngestEvent(heldTicket, event) {
  if (event?.type === "timeout") {
    return { outcome: heldTicket ? "unknown" : "ignored" };
  }
  const ticket = event?.result?.ticket;
  const own = typeof heldTicket === "string" && heldTicket !== ""
    && typeof ticket === "string" && ticket === heldTicket;
  return { outcome: own ? "completed" : "ignored" };
}

export function createIngestReceiptCache(limit = INGEST_RECEIPT_CACHE_LIMIT) {
  const receipts = new Map();
  return {
    observe(result) {
      const ticket = result?.ticket;
      if (typeof ticket !== "string" || ticket === "") return;
      if (receipts.has(ticket)) receipts.delete(ticket);
      receipts.set(ticket, result);
      while (receipts.size > limit) receipts.delete(receipts.keys().next().value);
    },
    take(ticket) {
      if (typeof ticket !== "string" || ticket === "" || !receipts.has(ticket)) return undefined;
      const result = receipts.get(ticket);
      receipts.delete(ticket);
      return result;
    },
  };
}

// Completes the pending control from a receipt already known to carry the
// held ticket, whether it arrived as a live frame or early in the cache.
export function completeIngest(state, result, { notice, scheduleRefresh }) {
  if (state.ingestTimer !== null) { clearTimeout(state.ingestTimer); state.ingestTimer = null; }
  state.ingestTicket = null;
  state.ingestPending = false;
  if (result?.error) notice(`Repository ingestion failed: ${result.error.message || "unknown error"}`, "error");
  else {
    const counts = ["scanned", "inserted", "skipped"]
      .map(key => Number.isFinite(result?.[key]) ? `${result[key]} ${key}` : null)
      .filter(Boolean).join(", ");
    notice(`Repository ingestion completed${counts ? `: ${counts}` : "."}`, "success");
  }
  scheduleRefresh();
}
