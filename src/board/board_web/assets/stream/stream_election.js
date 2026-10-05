// Cross-tab stream sharing: one leader per origin and token generation holds
// the Web Lock and runs the fetch reader; followers take parsed frames over a
// broadcast channel, show Live only on a fresh heartbeat, and steal the lock
// after two missed beats. Without locks or BroadcastChannel the tab stays solo.
const LOCK_NAME = "trufflepig-board-stream";
const CHANNEL_NAME = "trufflepig-board-stream";
export { LOCK_NAME as STREAM_LOCK_NAME, CHANNEL_NAME as STREAM_CHANNEL_NAME };
// Relayed frames received while idle buffer here until the snapshot lands, up
// to the server ring size; past that the tab resyncs instead of guessing.
const RELAY_BUFFER_CAP = 500;

export function createStreamElection({
  session, state, setConnection, authToken, heartbeatMs, freshnessMs, deliver, runFetch,
  rejoinAfterSteal,
}) {
  const shareable = (() => {
    try { return typeof navigator?.locks?.request === "function" && typeof BroadcastChannel === "function"; }
    catch (_) { return false; }
  })();
  function resetRelayBuffer() {
    session.relayBuffer = []; session.bufferOverran = false; session.bufferToken = null;
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
    if (!session.channel || session.channelName !== names.channel) {
      try { session.channel?.close(); } catch (_) { /* A missing close still replaces the channel. */ }
      session.channel = new BroadcastChannel(names.channel);
      session.channelName = names.channel;
      session.channel.onmessage = onRelay;
      // A moved listener starts clean: old-generation frames never apply here.
      resetRelayBuffer();
    }
    return session.channel;
  }
  function onRelay(message) {
    const frame = message.data;
    if (session.mode === "following" && !state.resyncing) {
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
    if (session.mode !== "idle" && !state.resyncing) return;
    // Idle or resyncing: validate exactly as above, then buffer for the next
    // snapshot. Status frames carry no event shape and fall out here.
    if (!frame || typeof frame.event !== "string" || typeof frame.data !== "string"
      || !(frame.id === null || typeof frame.id === "string")) return;
    if (session.bufferToken === null) session.bufferToken = typeof authToken === "function" ? authToken() : "";
    session.relayBuffer.push(frame);
    if (session.relayBuffer.length > RELAY_BUFFER_CAP) {
      session.relayBuffer = [];
      session.bufferOverran = true;
    }
  }

  // Applies frames buffered while idle once the snapshot lands. Returns false
  // when the buffer overran: the caller resyncs instead of trusting the gap.
  function drainRelayBuffer(cursor) {
    const pending = session.relayBuffer;
    const overran = session.bufferOverran;
    const token = typeof authToken === "function" ? authToken() : "";
    const generationChanged = session.bufferToken !== null && session.bufferToken !== token;
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
  // its fetch health; only a live one shows Live. A queued steal cannot be
  // cancelled — the spec bars signal with steal — so it stands and the leader
  // it evicts rejoins as a follower instead.
  function onHeartbeat(frame) {
    session.lastHeartbeat = Date.now();
    if (frame.state === "live") setConnection("Live", "live");
    else if (frame.state === "retrying") setConnection("Reconnecting…", "error");
  }

  function sendHeartbeat(names) {
    try {
      channelFor(names).postMessage({
        type: "status", state: session.leaderHealthy ? "live" : "retrying", watermark: state.watermark,
      });
    } catch (_) { /* A closed channel still leaves local delivery. */ }
  }

  // Followers watch for a silent leader: past the freshness window the tab
  // stops claiming Live and takes the lock by force, once per election.
  function startFreshnessTimer() {
    clearInterval(session.freshnessTimer);
    session.freshnessTimer = setInterval(() => {
      if (session.mode !== "following" || !session.lockNames) return;
      if (Date.now() - session.lastHeartbeat <= freshnessMs) return;
      setConnection("Reconnecting…", "error");
      if (!session.stealRequested) {
        try { requestLock(session.lockNames, true); } catch (_) { /* The next tick retries the steal. */ }
      }
    }, heartbeatMs);
  }

  // A steal carries no signal: the spec rejects the two together, so a queued
  // steal stays pending until granted. A synchronous throw leaves the previous
  // election fields alone so the next heartbeat or freshness tick retries.
  function requestLock(names, steal) {
    session.elect?.abort();
    const controller = new AbortController();
    const electionGeneration = state.streamGeneration;
    const options = steal ? { steal: true } : { signal: controller.signal };
    let granted = false;
    void navigator.locks.request(names.lock, options, () => {
      if (session.mode !== "following" || electionGeneration !== state.streamGeneration) return;
      granted = true;
      session.mode = "leading";
      becomeLeader(names);
      return new Promise(resolve => { session.lockEnd = resolve; });
    }).catch(error => {
      // Granted, then AbortError: the lock was stolen. A queued request aborts
      // only through stopStream, which resets the election itself.
      if (granted && error?.name === "AbortError") rejoinAfterSteal();
    });
    session.elect = controller;
    session.stealRequested = steal;
  }

  function becomeLeader(names) {
    clearInterval(session.freshnessTimer); session.freshnessTimer = null;
    session.stealRequested = false;
    session.leaderHealthy = false;
    sendHeartbeat(names);
    clearInterval(session.heartbeatTimer);
    session.heartbeatTimer = setInterval(() => { if (session.mode === "leading") sendHeartbeat(names); }, heartbeatMs);
    void runFetch(state.watermark ?? session.electionCursor, frame => channelFor(names).postMessage(frame));
  }

  // Releases the election half of a stream: timers, lock, and heartbeat state,
  // in stopStream order. The relay channel stays open so idle frames buffer.
  function resetElection() {
    clearInterval(session.heartbeatTimer); session.heartbeatTimer = null;
    clearInterval(session.freshnessTimer); session.freshnessTimer = null;
    session.mode = "idle";
    session.elect?.abort(); session.elect = null;
    session.lockEnd?.(); session.lockEnd = null;
    // The relay channel stays open: frames arriving while idle buffer for the
    // next snapshot instead of being lost across resyncs and rejoins.
    session.lockNames = null; session.electionCursor = null;
    session.lastHeartbeat = 0; session.stealRequested = false; session.leaderHealthy = false;
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

  return {
    shareable, streamNames, channelFor, drainRelayBuffer, resetRelayBuffer,
    startFreshnessTimer, requestLock, resetElection,
  };
}
