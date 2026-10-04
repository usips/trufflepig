export function createBoardStream(context) {
  const { state, privateFetch, setConnection, loadRoute, scheduleRefresh, parseBoardJson, addTickerEvent, addLiveEntry } = context;
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
  function stopStream() {
    state.streamGeneration++;
    state.stream?.abort(); state.stream = null;
    clearTimeout(state.reconnect); state.reconnect = null;
  }
  function startStream(cursor) {
    if (cursor === null || state.stream || state.resyncing) return;
    clearTimeout(state.reconnect);
    const controller = new AbortController();
    state.stream = controller;
    const generation = ++state.streamGeneration;
    void (async () => {
      let reader;
      try {
        const response = await privateFetch(`/api/v1/events?after=${encodeURIComponent(cursor)}`, {
          headers: { "Accept": "text/event-stream", "Last-Event-ID": cursor }, signal: controller.signal,
        });
        if (!response.ok || !response.body || !response.headers.get("Content-Type")?.startsWith("text/event-stream")) throw new Error(`Live connection failed (${response.status}).`);
        if (generation !== state.streamGeneration) return;
        setConnection("Live", "live");
        const decoder = new TextDecoder("utf-8", { fatal: true });
        let resync = false;
        const parse = parseSse(frame => {
          if (generation !== state.streamGeneration || resync) return;
          if (frame.event === "resync") { resync = true; return; }
          if (frame.event !== "board") return;
          if (!frame.id || !/^(0|[1-9]\d*)$/.test(frame.id)) throw new Error("Live event has an invalid sequence.");
          const event = parseBoardJson(frame.data);
          state.streamFailures = 0;
          state.watermark = frame.id;
          addTickerEvent(event);
          addLiveEntry?.(event);
          scheduleRefresh();
        });
        reader = response.body.getReader();
        while (generation === state.streamGeneration) {
          const chunk = await reader.read();
          if (chunk.done) { parse(decoder.decode()); break; }
          parse(decoder.decode(chunk.value, { stream: true }));
          if (resync) {
            state.watermark = null; state.resyncing = true;
            stopStream();
            try { await reader.cancel(); } catch (_) { /* Aborting fetch can error its body. */ }
            setConnection("Reloading snapshot…", "");
            await loadRoute(false, true);
            return;
          }
        }
        if (generation === state.streamGeneration) setConnection("Reconnecting…", "error");
      } catch (error) {
        if (error?.name !== "AbortError" && generation === state.streamGeneration) setConnection("Reconnecting…", "error");
      } finally {
        if (reader) { try { await reader.cancel(); } catch (_) { /* Stream already closed. */ } }
        if (generation === state.streamGeneration) {
          state.stream = null;
          const delay = Math.min(30000, 1000 * 2 ** Math.min(state.streamFailures++, 5));
          state.reconnect = setTimeout(() => startStream(state.watermark), delay);
        }
      }
    })();
  }

  return { parseSse, stopStream, startStream };
}
