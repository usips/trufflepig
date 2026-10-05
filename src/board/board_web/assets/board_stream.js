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
  } = context;
  function parseSse(onEvent) {
    let buffer = "", eventName = "message", eventId = null, data = [], frameBytes = 0;
    const encoder = new TextEncoder();
    const dispatch = () => {
      if (data.length) onEvent({ event: eventName, id: eventId, data: data.join("\n") });
      eventName = "message"; eventId = null; data = []; frameBytes = 0;
    };
    const line = text => {
      if (!text) { dispatch(); return; }
      frameBytes += encoder.encode(text).length + 1;
      if (frameBytes > 262144) throw new Error("Live event exceeded its limit.");
      if (text.startsWith(":")) return;
      const colon = text.indexOf(":");
      const field = colon < 0 ? text : text.slice(0, colon);
      let value = colon < 0 ? "" : text.slice(colon + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (field === "event") eventName = value;
      else if (field === "id" && !value.includes("\0")) eventId = value;
      else if (field === "data") data.push(value);
    };
    return chunk => {
      buffer += chunk;
      let start = 0;
      for (let index = 0; index < buffer.length; index++) {
        const character = buffer[index];
        if (character !== "\r" && character !== "\n") continue;
        if (character === "\r" && index === buffer.length - 1) break;
        line(buffer.slice(start, index));
        if (character === "\r" && buffer[index + 1] === "\n") index++;
        start = index + 1;
      }
      buffer = buffer.slice(start);
      if (frameBytes + encoder.encode(buffer).length > 262144) throw new Error("Live event buffer exceeded its limit.");
    };
  }

  // Cross-tab stream sharing. One leader per origin and token generation holds
  // the Web Lock and runs the fetch reader; every other tab follows parsed
  // frames over a broadcast channel instead of opening its own stream (browsers
  // cap connections per host, so each extra stream crowds out API calls). State
  // machine per tab: idle → following → leading → idle (on stopStream or resync
  // relay). The leader heartbeats every few seconds; followers show Live only on
  // a fresh heartbeat and steal the lock after two missed beats. A 401/403
  // expires the tab: it releases the lock, drops its token, and never retries.
  // Without navigator.locks or BroadcastChannel the tab stays solo: today's
  // behavior of one stream per tab, with no sharing attempted.
  const LOCK_NAME = "trufflepig-board-stream";
  const CHANNEL_NAME = "trufflepig-board-stream";
  // Relayed frames received while idle buffer here until the snapshot lands, up
  // to the server ring size; past that the tab resyncs instead of guessing.
  const RELAY_BUFFER_CAP = 500;
  const shareable = (() => {
    try { return typeof navigator?.locks?.request === "function" && typeof BroadcastChannel === "function"; }
    catch (_) { return false; }
  })();
  let mode = "idle", channel = null, channelName = "", elect = null, lockEnd = null;
  let lockNames = null, electionCursor = null;
  let heartbeatTimer = null, freshnessTimer = null, lastHeartbeat = 0;
  let stealRequested = false, leaderHealthy = false;
  let relayBuffer = [], bufferOverran = false, bufferToken = null;
  function resetRelayBuffer() {
    relayBuffer = []; bufferOverran = false; bufferToken = null;
  }

  // The lock and channel names for this tab's token: the first 16 hex chars of
  // its SHA-256, so tabs on different tokens never share a leader. Tabs without
  // a token, or without crypto.subtle, share the legacy un-namespaced names.
  async function streamNames() {
    const token = typeof authToken === "function" ? authToken() : "";
    const subtle = globalThis.crypto?.subtle;
    if (!token || typeof subtle?.digest !== "function") return { lock: LOCK_NAME, channel: CHANNEL_NAME };
    try {
      const digest = await subtle.digest("SHA-256", new TextEncoder().encode(token));
      const generation = [...new Uint8Array(digest)]
        .map(byte => byte.toString(16).padStart(2, "0")).join("").slice(0, 16);
      return { lock: `${LOCK_NAME}-${generation}`, channel: `${CHANNEL_NAME}-${generation}` };
    } catch (_) { return { lock: LOCK_NAME, channel: CHANNEL_NAME }; }
  }

  function channelFor(names) {
    if (!channel || channelName !== names.channel) {
      try { channel?.close(); } catch (_) { /* A missing close still replaces the channel. */ }
      channel = new BroadcastChannel(names.channel);
      channelName = names.channel;
      channel.onmessage = onRelay;
      // A moved listener starts clean: old-generation frames never apply here.
      resetRelayBuffer();
    }
    return channel;
  }
  function onRelay(message) {
    const frame = message.data;
    if (mode === "following" && !state.resyncing) {
      if (frame && frame.type === "status") {
        if (typeof frame.state !== "string"
          || !(frame.watermark === null || typeof frame.watermark === "string")) return;
        onHeartbeat(frame);
        return;
      }
      if (!frame || typeof frame.event !== "string" || typeof frame.data !== "string"
        || !(frame.id === null || typeof frame.id === "string")) return;
      try { deliver(frame); } catch (_) { /* A malformed relayed frame is dropped, never fatal. */ }
      return;
    }
    if (mode !== "idle" && !state.resyncing) return;
    // Idle or resyncing: validate exactly as above, then buffer for the next
    // snapshot. Status frames carry no event shape and fall out here.
    if (!frame || typeof frame.event !== "string" || typeof frame.data !== "string"
      || !(frame.id === null || typeof frame.id === "string")) return;
    if (bufferToken === null) bufferToken = typeof authToken === "function" ? authToken() : "";
    relayBuffer.push(frame);
    if (relayBuffer.length > RELAY_BUFFER_CAP) {
      relayBuffer = [];
      bufferOverran = true;
    }
  }

  // Applies frames buffered while idle once the snapshot lands. Returns false
  // when the buffer overran: the caller resyncs instead of trusting the gap.
  function drainRelayBuffer(cursor) {
    const pending = relayBuffer;
    const overran = bufferOverran;
    const token = typeof authToken === "function" ? authToken() : "";
    const generationChanged = bufferToken !== null && bufferToken !== token;
    resetRelayBuffer();
    // A new generation replays from this snapshot, so stale frames just drop.
    if (generationChanged) return true;
    if (overran) return false;
    for (const frame of pending) {
      try {
        if (frame.event === "board" && typeof frame.id === "string" && cursor !== null) {
          try {
            if (BigInt(frame.id) <= BigInt(cursor)) continue;
          } catch (_) { /* Non-numeric ids fall through; deliver drops them. */ }
        }
        if (deliver(frame)) break; // A buffered resync supersedes the rest.
      } catch (_) { /* Same as onRelay: a malformed buffered frame is dropped. */ }
    }
    return true;
  }

  // A valid status heartbeat proves the leader's event loop is alive, whatever
  // its fetch health; only a live one shows Live. A revived leader cancels a
  // pending steal so a healthy leader is never preempted.
  function onHeartbeat(frame) {
    lastHeartbeat = Date.now();
    if (stealRequested && lockNames) {
      try { requestLock(lockNames, false); } catch (_) { /* The next heartbeat retries the cancel. */ }
    }
    if (frame.state === "live") setConnection("Live", "live");
    else if (frame.state === "retrying") setConnection("Reconnecting…", "error");
  }

  function sendHeartbeat(names) {
    try {
      channelFor(names).postMessage({
        type: "status", state: leaderHealthy ? "live" : "retrying", watermark: state.watermark,
      });
    } catch (_) { /* A closed channel still leaves local delivery. */ }
  }

  // Followers watch for a silent leader: past the freshness window the tab
  // stops claiming Live and takes the lock by force, once per election.
  function startFreshnessTimer() {
    clearInterval(freshnessTimer);
    freshnessTimer = setInterval(() => {
      if (mode !== "following" || !lockNames) return;
      if (Date.now() - lastHeartbeat <= freshnessMs) return;
      setConnection("Reconnecting…", "error");
      if (!stealRequested) {
        try { requestLock(lockNames, true); } catch (_) { /* The next tick retries the steal. */ }
      }
    }, heartbeatMs);
  }

  // A synchronous throw leaves the previous election fields alone so the next
  // heartbeat or freshness tick retries; callers swallow it the same way.
  function requestLock(names, steal) {
    elect?.abort();
    const controller = new AbortController();
    const electionGeneration = state.streamGeneration;
    const options = steal
      ? { signal: controller.signal, steal: true }
      : { signal: controller.signal };
    void navigator.locks.request(names.lock, options, () => {
      if (mode !== "following" || electionGeneration !== state.streamGeneration) return;
      mode = "leading";
      becomeLeader(names);
      return new Promise(resolve => { lockEnd = resolve; });
    }).catch(() => { /* An aborted election ends with stopStream. */ });
    elect = controller;
    stealRequested = steal;
  }

  function becomeLeader(names) {
    clearInterval(freshnessTimer); freshnessTimer = null;
    stealRequested = false;
    leaderHealthy = false;
    sendHeartbeat(names);
    clearInterval(heartbeatTimer);
    heartbeatTimer = setInterval(() => { if (mode === "leading") sendHeartbeat(names); }, heartbeatMs);
    void runFetch(state.watermark ?? electionCursor, frame => channelFor(names).postMessage(frame));
  }

  // One frame handler for the leader's parsed frames and relayed frames alike.
  // Returns true on resync, the only frame that ends a fetch loop. Ingest and
  // other side frames carry no cursor and flow through with their type intact.
  function deliver(frame) {
    if (frame.event === "resync") { void runResync(); return true; }
    if (frame.event === "ingest") {
      try { onIngest?.(parseBoardJson(frame.data)); } catch (_) { /* A malformed receipt is dropped. */ }
      return false;
    }
    if (frame.event !== "board") return false;
    if (!frame.id || !/^(0|[1-9]\d*)$/.test(frame.id)) throw new Error("Live event has an invalid sequence.");
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
    clearInterval(heartbeatTimer); heartbeatTimer = null;
    clearInterval(freshnessTimer); freshnessTimer = null;
    mode = "idle";
    elect?.abort(); elect = null;
    lockEnd?.(); lockEnd = null;
    // The relay channel stays open: frames arriving while idle buffer for the
    // next snapshot instead of being lost across resyncs and rejoins.
    lockNames = null; electionCursor = null;
    lastHeartbeat = 0; stealRequested = false; leaderHealthy = false;
  }

  // The fetch reader: leaders pass a relay, solo tabs pass null. Reconnects
  // re-enter directly so a leader keeps its lock across transient failures.
  function runFetch(cursor, relay) {
    if (state.stream) return;
    const controller = new AbortController();
    state.stream = controller;
    const generation = ++state.streamGeneration;
    void (async () => {
      let reader;
      try {
        const response = await privateFetch(`/api/v1/events?after=${encodeURIComponent(cursor)}`, {
          headers: { "Accept": "text/event-stream", "Last-Event-ID": cursor }, signal: controller.signal,
        });
        if (generation !== state.streamGeneration) return;
        if (response.status === 401 || response.status === 403) {
          stopStream();
          setConnection(BOARD_AUTH_EXPIRED_MESSAGE, "expired");
          onAuthExpired?.();
          return;
        }
        if (!response.ok || !response.body
          || !response.headers.get("Content-Type")?.startsWith("text/event-stream")) {
          throw new Error(`Live connection failed (${response.status}).`);
        }
        leaderHealthy = true;
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
          leaderHealthy = false;
          setConnection("Reconnecting…", "error");
        }
      } catch (error) {
        if (error?.name !== "AbortError" && generation === state.streamGeneration) {
          leaderHealthy = false;
          setConnection("Reconnecting…", "error");
        }
      } finally {
        if (reader) { try { await reader.cancel(); } catch (_) { /* Stream already closed. */ } }
        if (generation === state.streamGeneration) {
          state.stream = null;
          const delay = Math.min(30000, 1000 * 2 ** Math.min(state.streamFailures++, 5));
          state.reconnect = setTimeout(() => {
            state.reconnect = null;
            if (mode !== "idle" && state.watermark !== null && !state.resyncing) runFetch(state.watermark, relay);
          }, delay);
        }
      }
    })();
  }

  function startStream(cursor) {
    if (cursor === null || state.stream || state.resyncing || mode !== "idle") return;
    clearTimeout(state.reconnect); state.reconnect = null;
    if (!shareable) { mode = "solo"; runFetch(cursor, null); return; }
    // Follow first: frames from the current leader apply while this tab waits
    // its turn on the lock. The browser grants the lock to one tab at a time.
    // Sharing names need the token digest, so the election follows async; the
    // tab claims Live only on a leader heartbeat, never eagerly here. Connecting
    // reports synchronously: the open channel can deliver a heartbeat first.
    mode = "following";
    electionCursor = cursor;
    // Drain synchronously: once following, live frames deliver directly, and a
    // later drain could apply buffered frames underneath a newer watermark.
    if (!drainRelayBuffer(cursor)) { void runResync(); return; }
    if (mode !== "following") return; // A buffered resync stopped the stream.
    setConnection("Connecting…", "");
    const setupGeneration = state.streamGeneration;
    void (async () => {
      let names;
      try {
        names = await streamNames();
      } catch (_) { names = { lock: LOCK_NAME, channel: CHANNEL_NAME }; }
      if (mode !== "following" || setupGeneration !== state.streamGeneration) return;
      try {
        lockNames = names;
        channelFor(names);
        lastHeartbeat = Date.now();
        startFreshnessTimer();
        requestLock(names, false);
      } catch (_) {
        // Sharing facilities that exist but throw leave the tab on its own stream.
        clearInterval(freshnessTimer); freshnessTimer = null;
        resetRelayBuffer();
        mode = "solo"; elect = null; runFetch(cursor, null);
      }
    })();
  }

  // Open the relay channel at setup, before the first snapshot read, so no
  // relayed frame is ever missed for lack of a listener. A later generation
  // change moves the listener through channelFor at election time.
  if (shareable) {
    void (async () => {
      let names;
      try {
        names = await streamNames();
      } catch (_) { names = { lock: LOCK_NAME, channel: CHANNEL_NAME }; }
      try { channelFor(names); } catch (_) { /* startStream falls back to solo. */ }
    })();
  }

  return { parseSse, stopStream, startStream };
}
