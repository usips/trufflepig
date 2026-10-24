// Free-text linkifier. One left-to-right tokenizer finds board references,
// commit ids, repository paths, http(s) URLs, and backtick spans; `linkify`
// renders them as anchors and `code` chips between text nodes. Output holds
// only text nodes, anchors, `code`, and empty copy buttons, so a container's
// textContent equals its source text. Never builds markup from strings.
import { boardUrlAllowed } from "./board_url_policy.js";

export const LINKIFY_MATCH_LIMIT = 200;
const PATH_EXTENSIONS = [
  "jsonl", "json", "toml", "yaml", "yml", "html", "css", "mjs", "cjs", "jsx", "js", "tsx", "ts",
  "rs", "md", "sh", "py", "sql", "txt", "lock", "luau", "lua", "wgsl", "ron", "svg", "php", "cs",
  "dm", "go", "cpp", "hpp", "c", "h",
].join("|");
const LEFT = String.raw`(?<![\p{L}\p{N}_.\/\\~@$#\-])`;
const RIGHT = String.raw`(?![\p{L}\p{N}_@]|[.\/][\p{L}\p{N}_])`;
const PATH_RIGHT = String.raw`(?![\p{L}\p{N}_@]|[.\/\-][\p{L}\p{N}_])`;
const TOKEN_PATTERN = new RegExp(`${LEFT}(?:${[
  String.raw`(?<code>\x60[^\x60\n]{1,200}\x60)`,
  String.raw`(?<url>https?:\/\/[^\s<>"'\x60]+)`,
  String.raw`(?<path>(?:[\w.\-]+\/)*[\w.\-]*(?<=[A-Za-z][\w\-]*)\.(?:${PATH_EXTENSIONS})`
    + String.raw`(?::\d+(?:-\d+)?|:[A-Za-z_]\w*(?:::\w+)*)?)${PATH_RIGHT}`,
  String.raw`(?<worker>P[1-9]\d*\.\d+\/[a-z0-9_\-]{1,32})${RIGHT}`,
  String.raw`(?<revision>P[1-9]\d*@\d+(?:\.\.\d+)?)${RIGHT}`,
  String.raw`(?<task>P[1-9]\d*\.\d+)${RIGHT}`,
  String.raw`(?<plan>P[1-9]\d*)${RIGHT}`,
  String.raw`(?<entry>E[1-9]\d*)${RIGHT}`,
  String.raw`(?<commit>[0-9a-fA-F]{64}|[0-9a-fA-F]{40})${RIGHT}`,
  String.raw`(?<short>(?=[0-9a-f]{0,11}[a-f])(?=[0-9a-f]{0,11}\d)[0-9a-f]{7,12})${RIGHT}`,
].join("|")})`, "gu");
const REF_KINDS = new Set(["worker", "revision", "task", "plan", "entry", "commit"]);
const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);
const SKIPPED_MARKUP = new Set(["A", "CODE", "PRE", "SCRIPT", "STYLE", "BUTTON"]);

// Drops trailing sentence punctuation and closers that have no opener.
function trimUrl(text) {
  let end = text.length;
  const count = (value, character) => value.split(character).length - 1;
  while (end > 0) {
    const last = text[end - 1], body = text.slice(0, end);
    if (".,;:!?*".includes(last)
      || last === ")" && count(body, "(") < count(body, ")")
      || last === "]" && count(body, "[") < count(body, "]")) end--;
    else break;
  }
  return text.slice(0, end);
}

// Tokens `{ kind, text, start, end }` in source order, at most `limit`.
export function boardTextTokens(text, limit = LINKIFY_MATCH_LIMIT) {
  const source = String(text ?? ""), tokens = [];
  const pattern = new RegExp(TOKEN_PATTERN);
  for (let match; tokens.length < limit && (match = pattern.exec(source));) {
    const [kind, value] = Object.entries(match.groups).find(([, group]) => group !== undefined);
    const token = kind === "url" ? trimUrl(value) : value;
    if (kind === "url") pattern.lastIndex = match.index + token.length;
    if (kind === "url" && !/^https?:\/\/[^/?#]/.test(token)) continue;
    tokens.push({ kind, text: token, start: match.index, end: match.index + token.length });
  }
  return tokens;
}

// A backtick span links only when its whole content is one token.
function codeContent(token) {
  const inner = token.text.slice(1, -1);
  const [whole] = boardTextTokens(inner, 1);
  return whole && whole.start === 0 && whole.end === inner.length ? whole : { kind: "text", text: inner };
}

function refKey(text) { return /^[0-9a-fA-F]{40,64}$/.test(text) ? text.toLowerCase() : text; }

// References `linkify` renders as inline links, normalized like refKey.
export function boardTextRefs(text) {
  const refs = new Set();
  for (const token of boardTextTokens(text)) {
    const linked = token.kind === "code" ? codeContent(token) : token;
    if (REF_KINDS.has(linked.kind)) refs.add(refKey(linked.text));
  }
  return refs;
}
export function boardRefLinkedInline(refs, ref) { return refs.has(refKey(String(ref))); }

// The trufflepig-agent command that opens a path token (path, path:line,
// path:start-end, or path:symbol) with ten lines of context before a line.
export function boardPathCommand(token) {
  const [, path, start, end, symbol] = /^(.*?)(?::(\d+)(?:-(\d+))?|:([A-Za-z_][\w:]*))?$/.exec(token);
  if (symbol) return `trufflepig-agent show 'sym:${symbol.split("::").pop()}'`;
  if (!start) return `trufflepig-agent map ${path}`;
  const first = Number(start), last = end ? Number(end) : first + 20;
  return `trufflepig-agent show path:${path}:${end ? first : Math.max(1, first - 10)}-${last}`;
}

// A same-origin board URL as `{ view, params }`, or null. Loopback aliases
// on the same port count as this board.
function boardUrlRoute(text, boardHost) {
  let url, board;
  try { url = new URL(text); board = new URL(`http://${boardHost}`); } catch (_) { return null; }
  const loopback = LOOPBACK_HOSTS.has(url.hostname) && LOOPBACK_HOSTS.has(board.hostname);
  if (!(url.host === board.host || loopback && url.port === board.port)
    || url.username || url.password || url.pathname !== "/" || url.search
    || url.hash && !url.hash.startsWith("#/")) return null;
  const [path, query = ""] = url.hash.slice(2).split("?");
  if (!/^[A-Za-z0-9@._\/-]*$/.test(path)) return null;
  const params = Object.fromEntries(new URLSearchParams(query));
  const view = /^P[1-9]\d*/.test(path) ? "plan" : /^E[1-9]\d*$/.test(path) ? "entry"
    : path.startsWith("edit/") ? "edit" : path || "overview";
  if (view === "plan" || view === "entry") params.ref = path;
  if (view === "edit") params.ref = path.slice(5);
  return { view, params };
}

function copyText(text) {
  try { globalThis.navigator?.clipboard?.writeText(text)?.catch?.(() => {}); } catch (_) { /* Silent. */ }
}

export function createBoardLinkify({ el, link, refLink }) {
  function pathChip(text, options) {
    const command = boardPathCommand(text);
    const chip = el("code", "linkify-path", text);
    chip.title = command;
    if (options.copy === false) return chip;
    const copy = el("button", "linkify-copy");
    copy.type = "button"; copy.title = `Copy: ${command}`;
    copy.setAttribute("aria-label", `Copy ${command}`);
    copy.addEventListener("click", event => {
      event.preventDefault(); event.stopPropagation?.(); copyText(command);
    });
    return [chip, copy];
  }
  function urlNode(text, context) {
    if (/token=/i.test(text)) return null;
    const route = boardUrlRoute(text, context.boardHost ?? globalThis.location?.host ?? "");
    if (route) return link(text, route.view, route.params, "linkify-url");
    if (!boardUrlAllowed(text)) return null;
    const anchor = el("a", "linkify-url", text);
    anchor.href = text; anchor.target = "_blank"; anchor.rel = "noopener noreferrer";
    return anchor;
  }
  function tokenNode(token, context, options) {
    const { kind, text } = token;
    if (REF_KINDS.has(kind)) {
      return refLink(text, text, kind === "commit" ? "mono linkify-ref" : "linkify-ref",
        { plan: context.plan, repo_key: context.repo_key });
    }
    if (kind === "short") {
      const item = link(text, "search", { q: `${text}*` }, "mono linkify-ref");
      item.title = `Search the board for commit ${text}`;
      return item;
    }
    if (kind === "path") return pathChip(text, options);
    if (kind === "url") return urlNode(text, context) || document.createTextNode(text);
    // A whole-span reference or URL renders as a code-styled link, not a
    // link nested in code; a refused URL stays code text.
    const inner = codeContent(token);
    const linked = ["text", "path"].includes(inner.kind) ? null : tokenNode(inner, context, options);
    const body = inner.kind === "path" ? pathChip(inner.text, options)
      : linked?.tagName === "A" ? Object.assign(linked, { className: `${linked.className} mono linkify-code` })
        : el("code", "linkify-code", inner.text);
    return [document.createTextNode("`"), body, document.createTextNode("`")].flat();
  }
  // options.maxCodepoints bounds a preview: text cuts at a codepoint, a
  // token is kept whole or dropped, and "…" marks the cut.
  function linkify(text, context = {}, options = {}) {
    const source = String(text ?? ""), fragment = document.createDocumentFragment();
    const max = options.maxCodepoints;
    const truncated = Number.isFinite(max) && Array.from(source).length > max;
    let budget = truncated ? max - 1 : Infinity, at = 0, open = true;
    const appendText = value => {
      const characters = Array.from(value);
      if (characters.length > budget) {
        fragment.append(document.createTextNode(characters.slice(0, budget).join(""))); open = false;
      } else if (value) {
        budget -= characters.length; fragment.append(document.createTextNode(value));
      }
    };
    for (const token of boardTextTokens(source)) {
      appendText(source.slice(at, token.start));
      if (!open) break;
      const cost = Array.from(token.text).length;
      if (cost > budget) { open = false; break; }
      budget -= cost; fragment.append(...[tokenNode(token, context, options)].flat()); at = token.end;
    }
    if (open) appendText(source.slice(at));
    if (truncated) fragment.append(document.createTextNode("…"));
    return fragment;
  }
  // Linkifies text nodes of server-sanitized markup outside a, code, and pre.
  function linkifyTextNodes(root, context = {}) {
    const texts = [];
    const walk = node => {
      for (const child of node.childNodes || []) {
        if (child.nodeType === 3) texts.push(child);
        else if (child.nodeType === 1 && !SKIPPED_MARKUP.has(child.tagName)) walk(child);
      }
    };
    walk(root);
    for (const node of texts) {
      if (boardTextTokens(node.textContent, 1).length) node.replaceWith(linkify(node.textContent, context));
    }
    return root;
  }
  function linkified(tag, className, text, context, options) {
    const item = el(tag, className);
    item.append(linkify(text, context, options));
    return item;
  }
  return { linkify, linkified, linkifyTextNodes };
}
