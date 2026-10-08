export const AGENT_MARKS = {
  claude: { glyph: "Cl", colorVar: "--mark-claude", image: null },
  codex: { glyph: "Cx", colorVar: "--mark-codex", image: null },
  kimi: { glyph: "Ki", colorVar: "--mark-kimi", image: null },
  grok: { glyph: "Gk", colorVar: "--mark-grok", image: null },
  gemini: { glyph: "Ge", colorVar: "--mark-gemini", image: null },
  qwen: { glyph: "Qw", colorVar: "--mark-qwen", image: null },
  muse: { glyph: "Mu", colorVar: "--mark-muse", image: null },
  human: { glyph: "Hu", colorVar: "--mark-human", image: null },
  unknown: { glyph: "?", colorVar: "--mark-unknown", image: null },
};

const SVG_NAMESPACE = "http://www.w3.org/2000/svg";

export function harnessVendor(harness) {
  if (harness === "cli") return "human";
  return Object.hasOwn(AGENT_MARKS, harness) ? harness : "unknown";
}

function svgElement(tag, attributes) {
  const element = document.createElementNS(SVG_NAMESPACE, tag);
  for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, value);
  return element;
}

export function agentMark(vendor, label) {
  const key = Object.hasOwn(AGENT_MARKS, vendor) ? vendor : "unknown";
  const mark = AGENT_MARKS[key];
  const accessibleLabel = String(label);
  if (mark.image) {
    const image = document.createElement("img");
    image.className = "agent-mark"; image.dataset.vendor = key;
    image.src = `/marks/${key}.svg`; image.alt = "";
    image.width = 16; image.height = 16; image.title = accessibleLabel;
    image.setAttribute("role", "img"); image.setAttribute("aria-label", accessibleLabel);
    return image;
  }
  const image = svgElement("svg", {
    class: "agent-mark", width: 16, height: 16, viewBox: "0 0 16 16",
    role: "img", "aria-label": accessibleLabel, focusable: "false",
  });
  image.dataset.vendor = key;
  const title = svgElement("title", {}); title.textContent = accessibleLabel;
  const square = svgElement("rect", {
    width: 16, height: 16, rx: 3, fill: `var(${mark.colorVar})`,
  });
  const glyph = svgElement("text", {
    x: 8, y: 8, "text-anchor": "middle", "dominant-baseline": "central",
    "font-family": "ui-sans-serif,system-ui,sans-serif", "font-size": 8,
    "font-weight": 700, fill: "var(--mark-glyph)",
  });
  glyph.textContent = mark.glyph;
  image.append(title, square, glyph);
  return image;
}

export function actorMark(record) {
  const actor = record.actor || {};
  const identity = `${actor.user || "unknown"}@${actor.host || "unknown"}`
    + `/${actor.session || "unknown"}`;
  return agentMark(record.vendor,
    `${record.model || "unclaimed"} · ${actor.harness || "unknown"} · ${identity}`);
}

export function recipientMark(recipient) {
  const identity = String(recipient);
  const actor = /^([^/@]+)@([^/@]+)\/([^/]+)\/([^/@]+)$/.exec(identity);
  if (!actor) return agentMark("unknown", `Assigned to ${identity}`);
  const [, user, host, harness, session] = actor;
  return actorMark({ vendor: harnessVendor(harness), actor: { user, host, harness, session } });
}
