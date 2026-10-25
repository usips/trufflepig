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
  now = Date.now, setInterval = globalThis.setInterval, clearInterval = globalThis.clearInterval,
}) {
  const shareable = (() => {
    try {
      return typeof navigator?.locks?.request === "function"
        && typeof BroadcastChannel === "function";
    }
    catch (_) { return false; }
  })();
  // Fetch retries advance streamGeneration; only resetting invalidates lock requests.
  let electionEpoch = 0;
  let stolenLeaderAwaitingSteal = false;
  function resetRelayBuffer() {
    session.relayBuffer = []; session.bufferOverran = false; session.bufferToken = null;
  }

  // The lock and channel names for this tab's token: the first 16 hex chars of
  // its SHA-256, so tabs on different tokens never share a leader. Without
  // crypto.subtle, tabs share the un-namespaced names.
  async function streamNames() {
    const token = authToken();
    const subtle = globalThis.crypto?.subtle;
    if (!token || typeof subtle?.digest !== "function") {
      return { lock: LOCK_NAME, channel: CHANNEL_NAME };
    }
    try {
      const digest = await subtle.digest("SHA-256", new TextEncoder().encode(token));
      const generation = [...new Uint8Array(digest)]
        .map(byte => byte.toString(16).padStart(2, "0")).join("").slice(0, 16);
      return { lock: `${LOCK_NAME}-${generation}`, channel: `${CHANNEL_NAME}-${generation}` };
    } catch (_) { return { lock: LOCK_NAME, channel: CHANNEL_NAME }; }
  }

  function channelFor(names) {
    if (!session.channel || session.channelName !== names.channel) {
      try { session.channel?.close(); }
      catch (_) { /* A missing close still replaces the channel. */ }
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
    if (session.bufferToken === null) session.bufferToken = authToken();
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
    const token = authToken();
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
    session.lastHeartbeat = now();
    if (frame.state === "live") setConnection("Live", "live");
    else if (frame.state === "retrying") setConnection("Reconnecting…", "error");
  }

  function sendHeartbeat(names) {
    try {
      channelFor(names).postMessage({
        type: "status", state: session.leaderHealthy ? "live" : "retrying",
        watermark: state.watermark,
      });
    } catch (_) { /* A closed channel still leaves local delivery. */ }
  }

  // Followers watch for a silent leader: past the freshness window the tab
  // stops claiming Live and takes the lock by force, one steal in flight at
  // a time; a rejection re-arms the next tick.
  function startFreshnessTimer() {
    clearInterval(session.freshnessTimer);
    session.freshnessTimer = setInterval(() => {
      if (session.mode !== "following" || !session.lockNames) return;
      if (now() - session.lastHeartbeat <= freshnessMs) return;
      setConnection("Reconnecting…", "error");
      if (!session.stealRequested) {
        try { requestLock(session.lockNames, true); }
        catch (_) { /* The next tick retries the steal. */ }
      }
    }, heartbeatMs);
  }

  // Steals omit signals and retain the queued election until a grant.
  // Rejections keep queue position and allow the next freshness tick to retry.
  // Synchronous throws preserve election fields for the next heartbeat or tick.
  function requestLock(names, steal) {
    const previousElect = session.elect;
    if (!steal) previousElect?.abort();
    const controller = new AbortController();
    const requestEpoch = electionEpoch;
    const options = steal ? { steal: true } : { signal: controller.signal };
    let granted = false, settled = false, release;
    void navigator.locks.request(names.lock, options, () => {
      if (settled || requestEpoch !== electionEpoch) return;
      const continuingLeader = steal && session.mode === "leading" && session.lockEnd !== null;
      if (session.mode !== "following" && !continuingLeader) return;
      granted = true;
      // Only a granted steal retires the queued request; a rejected one keeps it.
      previousElect?.abort();
      const previousRelease = session.lockEnd;
      const held = new Promise(resolve => { release = resolve; session.lockEnd = resolve; });
      stolenLeaderAwaitingSteal = false;
      if (steal) session.stealRequested = false;
      session.mode = "leading";
      if (!continuingLeader) becomeLeader(names);
      previousRelease?.();
      return held;
    }).catch(error => {
      settled = true;
      if (requestEpoch !== electionEpoch) return;
      if (granted && error?.name === "AbortError") {
        if (session.lockEnd !== release) return;
        // Our pending steal completes the handover; an external steal rejoins.
        if (session.stealRequested) stolenLeaderAwaitingSteal = true;
        else rejoinAfterSteal();
        return;
      }
      // A rejected steal leaves the queue position alone; the next tick retries.
      if (steal) {
        session.stealRequested = false;
        if (stolenLeaderAwaitingSteal) rejoinAfterSteal();
      }
    });
    if (!steal) session.elect = controller;
    session.stealRequested = steal;
  }

  function becomeLeader(names) {
    clearInterval(session.freshnessTimer); session.freshnessTimer = null;
    session.leaderHealthy = false;
    sendHeartbeat(names);
    clearInterval(session.heartbeatTimer);
    session.heartbeatTimer = setInterval(() => {
      if (session.mode === "leading") sendHeartbeat(names);
    }, heartbeatMs);
    void runFetch(state.watermark ?? session.electionCursor,
      frame => channelFor(names).postMessage(frame));
  }

  // Releases the election half of a stream: timers, lock, and heartbeat state,
  // in stopStream order. The relay channel stays open so idle frames buffer.
  function resetElection() {
    electionEpoch++;
    stolenLeaderAwaitingSteal = false;
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
  if (shareable && authToken()) {
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
