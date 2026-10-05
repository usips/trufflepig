import { createBoardReader } from "./board_reader.js";
import { createBoardStream, BOARD_AUTH_EXPIRED_MESSAGE } from "./stream/board_stream.js";
import { createBoardDom, decodeBoardFragment } from "./board_dom.js";
import { createBoardViews } from "./board_views.js";
import { createBoardRouting, routeFromLocation } from "./board_routing.js";
import { createBoardRenderLoop } from "./board_render_loop.js";
import { offeredTokenFromHash, resolveBootstrapToken, resolveAdoptionToken } from "./board_web_token.js";
import { completeIngest, createIngestReceiptCache, resolveIngestEvent } from "./board_ingest.js";
import { createDraftStores } from "./board_lru.js";
import { seenKey, readSeenMark, writeSeenMark } from "./board_seen.js";

const TOKEN_KEY = "trufflepig-board-token";
const TOKEN_MISMATCH_NOTICE = "This link offered a different board token; the tab kept its existing session.";
let stored = "";
try { stored = sessionStorage.getItem(TOKEN_KEY) || ""; } catch (_) { /* In-memory auth still works. */ }
const bootstrap = resolveBootstrapToken({
  locationHash: location.hash, locationHref: location.href, storedToken: stored,
});
let token = bootstrap.token;
// A mismatched offer stays in memory only: the stored session wins at load,
// but authorization expiry adopts the pending token without a second navigation.
let pendingToken = bootstrap.pending || "";
if (bootstrap.replaceUrl !== null) {
  history.replaceState(history.state, "", bootstrap.replaceUrl);
}
if (bootstrap.mismatch) {
  queueMicrotask(() => notice(TOKEN_MISMATCH_NOTICE, "error"));
} else if (bootstrap.persist) {
  try { sessionStorage.setItem(TOKEN_KEY, token); } catch (_) { /* In-memory auth still works. */ }
}
const apiVersion = Number(document.querySelector('meta[name="board-api"]')?.content);
// The human's seen mark: the highest event sequence this browser has had
// delivered, namespaced by board id; the session baseline renders the
// "new since you last saw" badge on the overview ticker.
const SEEN_KEY = seenKey(document.querySelector('meta[name="board-id"]')?.content);
const seenStorage = (() => {
  try { return localStorage; } catch (_) { return null; }
})();
const seenAtOpen = readSeenMark(seenStorage, SEEN_KEY);
const main = document.getElementById("main");
const connection = document.getElementById("connection");
const connectionAnnounce = document.getElementById("connection-announce");
const freshness = document.getElementById("freshness");
const draftStores = createDraftStores();
const state = {
  route: routeFromLocation(), generation: 0, navigation: 0, request: null, loadedAt: 0, clockOffsetMs: 0,
  overview: null, attention: null, watermark: null, stream: null,
  streamGeneration: 0, reconnect: null, streamFailures: 0,
  refreshTimer: null, refreshing: false, refreshAgain: false, drafts: draftStores.drafts, globalRefresh: null,
  resyncing: false, formDrafts: draftStores.formDrafts, formStatuses: draftStores.formStatuses,
  pendingForms: new Set(),
  seenAtOpen, seenHigh: seenAtOpen, ingestPending: false, ingestTicket: null, ingestTimer: null,
  ingestReceipts: createIngestReceiptCache(),
};
const columns = ["todo", "doing", "review", "done", "blocked"];
const postKinds = ["note", "answer", "decision", "question"];
const entryKinds = [
  "note", "progress", "review", "question", "answer", "decision", "divergence", "claim", "commit",
  "proposal", "feedback", "task", "hello", "create", "accept", "reject", "direct",
];

const dom = createBoardDom(state);
const { routeUrl, age } = dom;
// The render loop reads views, reader, and stream outputs through this wiring
// object: those factories close over loop functions at construction, so the
// loop's back-references land here once construction finishes, before the
// first route load or listener can run.
const wiring = {};
const loop = createBoardRenderLoop({
  state, dom, main, apiVersion, seenStorage, seenKey: SEEN_KEY,
  getToken: () => token, isExpired: () => expired, enterExpired, notice, setConnection, noteSeen,
  wiring,
});
const { navigate } = createBoardRouting({ state, routeUrl, notice, load: loop.loadRoute });
const views = createBoardViews({
  state, dom, notice, apiVersion, navigate, postKinds, entryKinds, columns,
  board: loop.board, jsonFetch: loop.jsonFetch, scheduleRefresh: loop.scheduleRefresh,
  submitMutation: loop.submitMutation, entryRecord: loop.entryRecord, collection: loop.collection,
  permitted: loop.permitted, nextAfter: loop.nextAfter, pageParams: loop.pageParams,
  queryFilters: loop.queryFilters, errorMessage: loop.errorMessage,
});

const { parseSse, stopStream, startStream } = createBoardStream({
  state, setConnection, noteSeen, onIngest,
  privateFetch: loop.privateFetch, loadRoute: loop.loadRoute, scheduleRefresh: loop.scheduleRefresh,
  parseBoardJson: loop.parseBoardJson, addTickerEvent: loop.addTickerEvent,
  authToken: () => token, onAuthExpired: used => enterExpired(used),
});

// Authorization expiry is one state wherever the 401/403 lands: the tab
// releases the lock, drops its token, and never retries until a fresh token
// is adopted. A 401 for a superseded token is stale and ignored, so parallel
// requests cannot expire a session adopted a tick earlier; a pending
// mismatched offer preempts expiry and reloads under the fresh token.
// Idempotent across the stream and JSON fetch paths.
let expired = false;
function enterExpired(failedToken) {
  if (expired) return;
  if (failedToken !== undefined && failedToken !== token) return;
  if (pendingToken) {
    token = pendingToken; pendingToken = "";
    try { sessionStorage.setItem(TOKEN_KEY, token); } catch (_) { /* In-memory auth still works. */ }
    notice("");
    setConnection("Connecting…", "");
    void loop.loadRoute(true);
    return;
  }
  expired = true;
  token = "";
  try { sessionStorage.removeItem(TOKEN_KEY); } catch (_) { /* In-memory auth still works. */ }
  stopStream();
  setConnection(BOARD_AUTH_EXPIRED_MESSAGE, "expired");
}

const { fetchRoute } = createBoardReader({
  state, dom, views, apiVersion,
  board: loop.board, jsonFetch: loop.jsonFetch, readOp: loop.readOp, snapshotSeq: loop.snapshotSeq,
  minimumSeq: loop.minimumSeq, cursorFromRoute: loop.cursorFromRoute,
  parseBoardJson: loop.parseBoardJson, queryFilters: loop.queryFilters,
});
wiring.attentionItems = views.attentionItems;
wiring.fetchRoute = fetchRoute;
wiring.startStream = startStream;

function noteSeen(seq) {
  const mark = writeSeenMark(seenStorage, SEEN_KEY, seq);
  if (mark !== null) state.seenHigh = mark;
}
function onIngest(result) {
  // Every frame is cached first: a receipt may publish before the tab's
  // own 202 arrives, and the 202 completes from the cache. Foreign
  // tickets (other tabs' flights, replays from before this tab queued)
  // are ignored; only the held ticket completes the control.
  state.ingestReceipts.observe(result);
  if (resolveIngestEvent(state.ingestTicket, { type: "receipt", result }).outcome === "ignored") return;
  completeIngest(state, result, { notice, scheduleRefresh: loop.scheduleRefresh });
}

function notice(message, tone = "") {
  const item = document.getElementById("notice");
  item.textContent = message;
  item.dataset.tone = tone;
  item.hidden = !message;
}
// The indicator itself stays out of the live region: only losing an
// established connection, authorization expiry, and recovering from an
// outage are announced, never routine (re)connect ticks or follower "Live".
let connectionAnnounced = "";
function setConnection(label, mode) {
  connection.textContent = label; connection.dataset.state = mode;
  if (mode === connectionAnnounced) return;
  if (mode === "expired") {
    connectionAnnounce.textContent = label;
  } else if (mode === "error" && connectionAnnounced === "live") {
    connectionAnnounce.textContent = "Board connection lost. Reconnecting.";
  } else if (mode === "live" && (connectionAnnounced === "error" || connectionAnnounced === "expired")) {
    connectionAnnounce.textContent = "Board connection restored.";
  }
  connectionAnnounced = mode;
}
document.addEventListener("click", event => {
  if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey
    || event.altKey) return;
  const fragmentLink = event.target.closest("a[href^='#']");
  if (fragmentLink && !fragmentLink.getAttribute("href").startsWith("#/")) {
    event.preventDefault();
    const heading = decodeBoardFragment(fragmentLink.getAttribute("href").slice(1));
    history.replaceState({}, "", routeUrl(state.route.view, { ...state.route, heading }));
    state.route.heading = heading; document.getElementById(heading)?.scrollIntoView?.({ block: "start" });
    if (heading === "main") main.focus({ preventScroll: true }); return;
  }
  const anchor = event.target.closest("a[data-nav],a[data-route]");
  if (!anchor || event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey
    || event.shiftKey || event.altKey) return;
  event.preventDefault();
  const target = new URL(anchor.href, location.href);
  history.pushState({}, "", target.pathname + target.search + target.hash);
  const next = routeFromLocation();
  if (routeUrl(next.view, { ...next, heading: "" }) === routeUrl(state.route.view, { ...state.route, heading: "" })) {
    state.route = next; document.getElementById(next.heading)?.scrollIntoView?.({ block: "start" }); return;
  }
  state.navigation++;
  state.route = routeFromLocation(); notice(""); void loop.loadRoute(true);
});
window.addEventListener("hashchange", () => {
  // Fresh-token offered by navigation: only an expired tab adopts it. A live
  // tab still strips the token from the URL at once, so it never lingers in
  // the address bar or history, and holds it pending for a later expiry.
  const adoption = resolveAdoptionToken({ locationHash: location.hash, expired });
  if (adoption !== null) {
    token = adoption; expired = false;
    try { sessionStorage.setItem(TOKEN_KEY, token); } catch (_) { /* In-memory auth still works. */ }
    history.replaceState(history.state, "", location.pathname + location.search);
    state.navigation++; state.route = routeFromLocation(); void loop.loadRoute(true); return;
  }
  const stray = expired ? null : offeredTokenFromHash(location.hash);
  if (stray !== null) {
    pendingToken = stray;
    history.replaceState(history.state, "", location.pathname + location.search);
    notice(TOKEN_MISMATCH_NOTICE, "error");
    state.navigation++; state.route = routeFromLocation(); void loop.loadRoute(true); return;
  }
  state.navigation++; state.route = routeFromLocation(); void loop.loadRoute(true);
});
window.addEventListener("pagehide", () => { stopStream(); state.request?.abort(); state.globalRefresh?.abort(); });
window.addEventListener("pageshow", event => { if (event.persisted) void loop.loadRoute(false, true); });
document.getElementById("refresh").addEventListener("click", () => void loop.loadRoute(false));
setInterval(() => {
  for (const item of document.querySelectorAll("[data-age]")) item.textContent = age(item.dataset.age);
  freshness.textContent = state.loadedAt ? `Updated ${Math.floor((Date.now() - state.loadedAt) / 1000)}s ago` : "";
}, 1000);
setInterval(() => {
  if (document.visibilityState !== "hidden") {
    if (state.route.view !== "overview") void loop.refreshOverview();
    loop.scheduleRefresh();
  }
}, 15000);
document.addEventListener("visibilitychange", () => {
  // A hidden tab yields its stream: a hidden leader's timers are throttled,
  // so it releases the lock and a visible tab takes over. Foregrounding
  // refreshes and rejoins the stream; a stale watermark resyncs by itself.
  if (document.visibilityState === "hidden") stopStream();
  else if (document.visibilityState === "visible") loop.scheduleRefresh();
});
void loop.loadRoute(true);
