// Pure ingest-receipt decision for the board tab: a tab completes only on the
// ticket its own 202 returned. Foreign tickets (other tabs' flights, replays
// from before this tab queued) are ignored, and a pending ticket still
// unconfirmed after thirty seconds resolves to unknown, never completed.
export const INGEST_TIMEOUT_MS = 30000;
export const INGEST_UNKNOWN_MESSAGE = "ingest result unknown";

export function resolveIngestEvent(heldTicket, event) {
  if (event?.type === "timeout") {
    return { outcome: heldTicket ? "unknown" : "ignored" };
  }
  const ticket = event?.result?.ticket;
  const own = typeof heldTicket === "string" && heldTicket !== ""
    && typeof ticket === "string" && ticket === heldTicket;
  return { outcome: own ? "completed" : "ignored" };
}
