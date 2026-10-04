export function createBoardStream(context) {
  const {
    state, privateFetch, setConnection, loadRoute, scheduleRefresh, parseBoardJson, addTickerEvent,
    addLiveEntry, noteSeen, onIngest,
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

  // Cross-tab stream sharing. One leader per origin holds the Web Lock and runs
  // the fetch reader; every other tab follows parsed frames over a broadcast
  // channel instead of opening its own stream (browsers cap connections per
  // host, so each extra stream crowds out API calls). State machine per tab:
  //   idle → following → leading → idle (on stopStream or resync relay)
  // The lock is requested without ifAvailable: it is granted when the leader's
  // tab closes or releases, promoting the next waiter (failover). A promoted
  // tab restarts the stream from its own watermark; every tab dedupes by frame
  // id, so overlapping redelivery after a leadership change is harmless. A
  // resync frame, received or relayed, sends every tab through the resync path.
  // Without navigator.locks or BroadcastChannel the tab stays solo: today's
  // behavior of one stream per tab, with no sharing attempted.
  const LOCK_NAME = "trufflepig-board-stream";
  const CHANNEL_NAME = "trufflepig-board-stream";
  const shareable = (() => {
    try { return typeof navigator?.locks?.request === "function" && typeof BroadcastChannel === "function"; }
    catch (_) { return false; }
  })();
  let mode = "idle", channel = null, elect = null, lockEnd = null;

  function relayChannel() {
    if (!channel) {
      channel = new BroadcastChannel(CHANNEL_NAME);
      channel.onmessage = onRelay;
    }
    return channel;
  }
  function onRelay(message) {
    if (mode !== "following") return;
    const frame = message.data;
    if (!frame || typeof frame.event !== "string" || typeof frame.data !== "string"
      || !(frame.id === null || typeof frame.id === "string")) return;
    try { deliver(frame); } catch (_) { /* A malformed relayed frame is dropped, never fatal. */ }
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
    addLiveEntry?.(event);
    scheduleRefresh();
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
    mode = "idle";
    elect?.abort(); elect = null;
    lockEnd?.(); lockEnd = null;
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
        if (!response.ok || !response.body
          || !response.headers.get("Content-Type")?.startsWith("text/event-stream")) {
          throw new Error(`Live connection failed (${response.status}).`);
        }
        if (generation !== state.streamGeneration) return;
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
        if (!resync && generation === state.streamGeneration) setConnection("Reconnecting…", "error");
      } catch (error) {
        if (error?.name !== "AbortError" && generation === state.streamGeneration) {
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
    try {
      // Follow first: frames from the current leader apply while this tab waits
      // its turn on the lock. The browser grants the lock to one tab at a time.
      mode = "following";
      relayChannel();
      setConnection("Live", "live");
      const generation = state.streamGeneration;
      const controller = new AbortController();
      elect = controller;
      void navigator.locks.request(LOCK_NAME, { signal: controller.signal }, () => {
        if (mode !== "following" || generation !== state.streamGeneration) return;
        mode = "leading";
        void runFetch(state.watermark ?? cursor, frame => relayChannel().postMessage(frame));
        return new Promise(resolve => { lockEnd = resolve; });
      }).catch(() => { /* An aborted election ends with stopStream. */ });
    } catch (_) {
      // Sharing facilities that exist but throw leave the tab on its own stream.
      mode = "solo"; elect = null; runFetch(cursor, null);
    }
  }

  return { parseSse, stopStream, startStream };
}
