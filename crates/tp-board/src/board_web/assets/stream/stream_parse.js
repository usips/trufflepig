export function parseSse(onEvent) {
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
