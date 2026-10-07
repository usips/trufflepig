import { parseSse } from "./stream_parse.js";
import { STREAM_CHANNEL_NAME, STREAM_LOCK_NAME, createStreamElection } from "./stream_election.js";

// One expired state, one message: a 401/403 anywhere converges here, and the
// bootstrap announces this label rather than a generic reconnect warning.
export const BOARD_AUTH_EXPIRED_MESSAGE = "Authorization expired. Run `trufflepig board web`.";

// Delivery always advances the watermark, ticker, and seen mark; only the
// page re-fetch is gated. Feedback lists only feedback-kind events, and a
// plan-filtered search only its plan; a query-only search cannot exclude any
// event, so it refetches like every other route.
export function routeRefreshesOn(route, event) {
  if (route?.view === "feedback") return event?.kind === "feedback";
  if (route?.view === "search") return !route.plan || String(event?.plan || "") === route.plan;
  return true;
}

export function createBoardStream(context) {
  const {
    state, privateFetch, setConnection, loadRoute, scheduleRefresh, parseBoardJson, addTickerEvent,
    noteSeen, onIngest,
    authToken, onAuthExpired, heartbeatMs = 5000, freshnessMs = 10000,
    now = Date.now, setInterval = globalThis.setInterval, clearInterval = globalThis.clearInterval,
  } = context;
  if (typeof authToken !== "function") throw new TypeError("createBoardStream requires authToken.");
  // Election and fetch-reader state shared with the stream election module.
  const session = {
    mode: "idle", channel: null, channelName: "", elect: null, lockEnd: null,
    lockNames: null, electionCursor: null, deferredCursor: null,
    heartbeatTimer: null, freshnessTimer: null, lastHeartbeat: 0,
    stealRequested: false, leaderHealthy: false,
    relayBuffer: [], bufferOverran: false, bufferToken: null,
  };

  // One frame handler for the leader's parsed frames and relayed frames alike.
  // Returns true on resync, the only frame that ends a fetch loop. Ingest and
  // other side frames carry no cursor and flow through with their type intact.
  function deliver(frame) {
    if (frame.event === "resync") { void runResync(); return true; }
    if (frame.event === "ingest") {
      try { onIngest?.(parseBoardJson(frame.data)); }
      catch (_) { /* A malformed receipt is dropped. */ }
      return false;
    }
    if (frame.event !== "board") return false;
    if (!frame.id || !/^(0|[1-9]\d*)$/.test(frame.id)) {
      throw new Error("Live event has an invalid sequence.");
    }
    if (state.watermark !== null && BigInt(frame.id) <= BigInt(state.watermark)) return false;
    const event = parseBoardJson(frame.data);
    state.streamFailures = 0;
    state.watermark = frame.id;
    noteSeen?.(frame.id);
    addTickerEvent(event);
    if (routeRefreshesOn(state.route, event)) scheduleRefresh();
    return false;
  }
  async function runResync() {
    if (state.resyncing) return;
    state.watermark = null; state.resyncing = true;
    stopStream();
    setConnection("Reloading snapshot…", "");
    await loadRoute(false, true);
  }

  function stopStream() {
    state.streamGeneration++;
    state.stream?.abort(); state.stream = null;
    clearTimeout(state.reconnect); state.reconnect = null;
    session.deferredCursor = null;
    election.resetElection();
  }

  function authorizedToken() {
    const token = authToken();
    if (!token) {
      stopStream();
      setConnection("Authorization required", "error");
    }
    return token;
  }

  // A stolen lock means another tab leads now: stop fetching and heartbeating,
  // then rejoin the election as a follower.
  function rejoinAfterSteal() {
    const cursor = state.watermark ?? session.electionCursor;
    stopStream();
    if (cursor !== null) startStream(cursor);
  }

  // The fetch reader: leaders pass a relay, solo tabs pass null. Reconnects
  // re-enter directly so a leader keeps its lock across transient failures.
  function runFetch(cursor, relay) {
    if (state.stream) return;
    const sent = authorizedToken();
    if (!sent) return;
    const controller = new AbortController();
    state.stream = controller;
    const generation = ++state.streamGeneration;
    void (async () => {
      let reader;
      try {
        const response = await privateFetch(`/api/v1/events?after=${encodeURIComponent(cursor)}`, {
          headers: { "Accept": "text/event-stream", "Last-Event-ID": cursor },
          signal: controller.signal,
        });
        if (generation !== state.streamGeneration) return;
        if (response.status === 401 || response.status === 403) {
          stopStream();
          setConnection(BOARD_AUTH_EXPIRED_MESSAGE, "expired");
          onAuthExpired?.(sent);
          return;
        }
        if (!response.ok || !response.body
          || !response.headers.get("Content-Type")?.startsWith("text/event-stream")) {
          throw new Error(`Live connection failed (${response.status}).`);
        }
        session.leaderHealthy = true;
        setConnection("Live", "live");
        const decoder = new TextDecoder("utf-8", { fatal: true });
        let resync = false;
        const parse = parseSse(frame => {
          if (generation !== state.streamGeneration || resync) return;
          if (relay) {
            try { relay({ event: frame.event, id: frame.id, data: frame.data }); }
            catch (_) { /* A closed channel still leaves local delivery. */ }
          }
          if (deliver(frame)) resync = true;
        });
        reader = response.body.getReader();
        while (generation === state.streamGeneration && !resync) {
          const chunk = await reader.read();
          if (chunk.done) { parse(decoder.decode()); break; }
          parse(decoder.decode(chunk.value, { stream: true }));
        }
        if (!resync && generation === state.streamGeneration) {
          session.leaderHealthy = false;
          setConnection("Reconnecting…", "error");
        }
      } catch (error) {
        if (error?.name !== "AbortError" && generation === state.streamGeneration) {
          session.leaderHealthy = false;
          setConnection("Reconnecting…", "error");
        }
      } finally {
        if (reader) { try { await reader.cancel(); } catch (_) { /* Stream already closed. */ } }
        if (generation === state.streamGeneration) {
          state.stream = null;
          const delay = Math.min(30000, 1000 * 2 ** Math.min(state.streamFailures++, 5));
          state.reconnect = setTimeout(() => {
            state.reconnect = null;
            if (session.mode !== "idle" && state.watermark !== null && !state.resyncing) {
              runFetch(state.watermark, relay);
            }
          }, delay);
        }
      }
    })();
  }

  function startStream(cursor) {
    if (cursor === null || state.stream || state.resyncing || session.mode !== "idle") return;
    if (!authorizedToken()) return;
    // A hidden tab never leads: throttled background timers would stall its
    // heartbeat for every follower. Election resumes on visibilitychange.
    if (document.visibilityState === "hidden") { session.deferredCursor = cursor; return; }
    session.deferredCursor = null;
    clearTimeout(state.reconnect); state.reconnect = null;
    if (!election.shareable) { session.mode = "solo"; runFetch(cursor, null); return; }
    // Follow first: frames from the current leader apply while this tab waits
    // its turn on the lock. The browser grants the lock to one tab at a time.
    // Sharing names need the token digest, so the election follows async; the
    // tab claims Live only on a leader heartbeat, never eagerly here. Connecting
    // reports synchronously: the open channel can deliver a heartbeat first.
    session.mode = "following";
    session.electionCursor = cursor;
    // Drain synchronously: once following, live frames deliver directly, and a
    // later drain could apply buffered frames underneath a newer watermark.
    if (!election.drainRelayBuffer(cursor)) { void runResync(); return; }
    if (session.mode !== "following") return; // A buffered resync stopped the stream.
    setConnection("Connecting…", "");
    const setupGeneration = state.streamGeneration;
    void (async () => {
      let names;
      try {
        names = await election.streamNames();
      } catch (_) { names = { lock: STREAM_LOCK_NAME, channel: STREAM_CHANNEL_NAME }; }
      if (session.mode !== "following" || setupGeneration !== state.streamGeneration) return;
      try {
        session.lockNames = names;
        election.channelFor(names);
        session.lastHeartbeat = now();
        election.startFreshnessTimer();
        election.requestLock(names, false);
      } catch (_) {
        // Sharing facilities that exist but throw leave the tab on its own stream.
        clearInterval(session.freshnessTimer); session.freshnessTimer = null;
        election.resetRelayBuffer();
        session.mode = "solo"; session.elect = null; runFetch(cursor, null);
      }
    })();
  }

  const election = createStreamElection({
    session, state, setConnection, authToken, heartbeatMs, freshnessMs, deliver, runFetch,
    rejoinAfterSteal, now, setInterval, clearInterval,
  });

  // A tab that deferred its election while hidden starts when shown again.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "hidden" && session.deferredCursor !== null) {
      startStream(session.deferredCursor);
    }
  });

  return { parseSse, stopStream, startStream };
}
