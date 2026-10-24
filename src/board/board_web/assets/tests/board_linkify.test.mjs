import { beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardViews } from "../board_views.js";
import { boardPathCommand, boardTextRefs, boardTextTokens, LINKIFY_MATCH_LIMIT } from "../board_linkify.js";

installDomShim();
beforeEach(() => resetDomShim());

const BOARD = { boardHost: "127.0.0.1:44977" };
const TOKEN = "0123456789abcdef".repeat(4);
const E6512 = "Re E6483: Josh confirmed 2026-10-15 in this session that E6121 (E6099 flight survival) and "
  + "E6145 (E6100 transfer-row cancellation causes) are his rulings. Master c4732d1b4 still diverges: "
  + "spray.rs:83 PendingSpray.sprayer, spray_cancel.rs target_transferred/sprayer_transferred, "
  + "transfer_purges_spray.rs asserting sprayer_transferred, and docs/architecture/map-instancing.md:194 "
  + "'rows keep their own start cause'. The follow-up is carved as P10.70 (todo, unclaimed). P10.21 stays "
  + "in review until P10.70 lands, then moves to done.";

function setup() {
  const state = { route: { view: "overview" }, clockOffsetMs: 0, formDrafts: new Map(),
    formStatuses: new Map(), pendingForms: new Set(), seenAtOpen: null, projects: [] };
  return { state, dom: createBoardDom(state) };
}
function render(text, context = BOARD, options) {
  const { dom } = setup();
  return dom.linkified("p", "body", text, context, options);
}
function nodes(root) { return root.childNodes.flatMap(node => [node, ...(node.childNodes ? nodes(node) : [])]); }
function anchors(root) { return nodes(root).filter(node => node.tagName === "A"); }
function kinds(text) { return boardTextTokens(text).map(token => `${token.kind}:${token.text}`); }

describe("board text tokens", () => {
  it("finds every reference, commit, and path in a real entry", () => {
    assert.deepEqual(kinds(E6512), [
      "entry:E6483", "entry:E6121", "entry:E6099", "entry:E6145", "entry:E6100", "short:c4732d1b4",
      "path:spray.rs:83", "path:spray_cancel.rs", "path:transfer_purges_spray.rs",
      "path:docs/architecture/map-instancing.md:194", "task:P10.70", "task:P10.21", "task:P10.70",
    ]);
    assert.deepEqual(kinds("worker P10.70/parser, P7@3, P7@1..3 and P7. See sim/movement.rs:propose_surface_cross"
      + ` or ${"a".repeat(40)}.`), ["worker:P10.70/parser", "revision:P7@3", "revision:P7@1..3", "plan:P7",
      "path:sim/movement.rs:propose_surface_cross", `commit:${"a".repeat(40)}`]);
  });

  it("leaves prose, versions, emails, and absolute paths as text", () => {
    const prose = "A decade-old facade; deadbeef is no commit, 1234567 neither. Version 1.2.3, v1.2.c, 3.c, "
      + "e.g. and i.e. stay prose. Mail owner@example.test or a@host.rs. #12 and #E4 are ambiguous; "
      + "P7.3.1, E2E, P2P, MP3, worktree-agent-a9a0685, /home/josh/.cache/run/x.md, ~/.cache/z.rs, "
      + "file:///etc/passwd.md, javascript:alert(1), data:text/html,boom and vscode://file/x.rs too.";
    assert.deepEqual(kinds(prose), []);
  });

  it("trims trailing punctuation and unbalanced closers from URLs", () => {
    assert.deepEqual(kinds("(see https://example.com/a_(b)) and https://example.com/x). Or http://h.test/p?q=1;"),
      ["url:https://example.com/a_(b)", "url:https://example.com/x", "url:http://h.test/p?q=1"]);
  });

  it("caps matches per call", () => {
    assert.equal(boardTextTokens(Array.from({ length: 300 }, (_, at) => `E${at + 1}`).join(" ")).length,
      LINKIFY_MATCH_LIMIT);
  });

  it("names the trufflepig-agent command for each path form", () => {
    assert.equal(boardPathCommand("spray.rs:83"), "trufflepig-agent show path:spray.rs:73-103");
    assert.equal(boardPathCommand("docs/x.md:2"), "trufflepig-agent show path:docs/x.md:1-22");
    assert.equal(boardPathCommand("src/a.rs:10-20"), "trufflepig-agent show path:src/a.rs:10-20");
    assert.equal(boardPathCommand("sim/movement.rs:propose_surface_cross"),
      "trufflepig-agent show 'sym:propose_surface_cross'");
    assert.equal(boardPathCommand("docs/board-web-ui.md"), "trufflepig-agent map docs/board-web-ui.md");
  });
});

describe("linkify rendering", () => {
  it("links a real entry inline while textContent stays the body", () => {
    const body = render(E6512, { ...BOARD, plan: "P10" });
    assert.equal(body.textContent, E6512);
    const hrefs = anchors(body).map(item => [item.textContent, item.href]);
    assert.deepEqual(hrefs.slice(0, 6), [
      ["E6483", "/#/E6483"], ["E6121", "/#/E6121"], ["E6099", "/#/E6099"], ["E6145", "/#/E6145"],
      ["E6100", "/#/E6100"], ["c4732d1b4", "/#/search?q=c4732d1b4*"],
    ]);
    assert.deepEqual(hrefs.at(-1), ["P10.70", "/#/P10?tab=tasks&task=P10.70"]);
    assert.ok(anchors(body).every(item => item.dataset.nav === "true"));
    const chips = body.querySelectorAll("code.linkify-path");
    assert.deepEqual(chips.map(chip => chip.title), [
      "trufflepig-agent show path:spray.rs:73-103", "trufflepig-agent map spray_cancel.rs",
      "trufflepig-agent map transfer_purges_spray.rs",
      "trufflepig-agent show path:docs/architecture/map-instancing.md:184-214",
    ]);
    const copies = body.querySelectorAll("button.linkify-copy");
    assert.equal(copies.length, 4);
    assert.ok(copies.every(copy => copy.textContent === "" && copy.type === "button"));
  });

  it("routes full commits to the entry's plan and refs through refLink", () => {
    const oid = "c".repeat(40);
    const body = render(`Landed ${oid} for P7.3/parser`, { plan: "P7", repo_key: "r1" });
    assert.deepEqual(anchors(body).map(item => item.href), [
      `/#/P7?tab=commits&oid=${oid}&repo=r1`, "/#/P7?tab=tasks&task=P7.3&focus=worker-P7.3-parser",
    ]);
  });

  it("turns this board's URLs into internal routes and refuses token links", () => {
    const text = "Tried: Open http://127.0.0.1:44977/#/P10?tab=tasks&task=P10.71.";
    const body = render(text);
    const [link] = anchors(body);
    assert.equal(link.href, "/#/P10?tab=tasks&task=P10.71");
    assert.equal(link.dataset.nav, "true");
    assert.equal(link.target, undefined);
    assert.equal(body.textContent, text);
    assert.equal(anchors(render("http://localhost:44977/#/E5")).at(0).href, "/#/E5");
    for (const refused of [
      `http://127.0.0.1:44977/#token=${TOKEN}`, `http://127.0.0.1:44977/#/P7?token=${TOKEN}`,
      `http://127.0.0.1:44977/?ref=P7#token=${TOKEN}`, `https://example.com/#token=${TOKEN}`,
      "http://127.0.0.1:9999/#/P7", "http://user@127.0.0.1:44977/#/P7", "http://127.0.0.1:44977/x#/P7",
    ]) {
      const node = render(refused);
      assert.equal(anchors(node).length, 0, refused);
      assert.equal(node.textContent, refused);
    }
  });

  it("opens external links in a new tab without a referrer", () => {
    const body = render("See https://example.com/x). Done");
    const [link] = anchors(body);
    assert.equal(link.href, "https://example.com/x");
    assert.equal(link.target, "_blank");
    assert.equal(link.rel, "noopener noreferrer");
    assert.equal(body.textContent, "See https://example.com/x). Done");
  });

  it("never emits script-capable hrefs or markup from text", () => {
    const hostile = [
      "javascript:alert(1) JAVASCRIPT:alert(1) data:text/html,<b>x</b> vbscript:x file:///etc/x.md",
      "vscode://file/home/x.rs <script>alert(1)</script> <img src=x onerror=alert(1)>",
      `https://evil.test/"><script>alert(1)</script> src/x".rs 'docs/a'.md`,
      "`javascript:alert(1)` http://evil.test/javascript:alert(1) https://ｅｖｉｌ.test/ http://[::1]/x",
    ].join("\n");
    const body = render(hostile);
    assert.equal(body.textContent, hostile);
    for (const node of nodes(body)) {
      assert.ok(node.nodeType === 3 || ["A", "CODE", "BUTTON"].includes(node.tagName), node.tagName);
    }
    for (const link of anchors(body)) assert.match(link.href, /^(https?:\/\/|\/#\/)/, link.href);
    assert.deepEqual(anchors(body).map(item => item.href),
      ["https://evil.test/", "http://evil.test/javascript:alert(1)"]);
  });

  it("renders backtick spans as code and links a whole-span reference or path", () => {
    const text = "Run `cargo test --lib` then `P7.3` and `src/x.rs:10-20` but not `see E5 here`.";
    const body = render(text);
    assert.equal(body.textContent, text);
    const spans = body.children.filter(node => node.tagName !== "BUTTON");
    assert.deepEqual(spans.map(node => `${node.tagName} ${node.className}`), ["CODE linkify-code",
      "A linkify-ref mono linkify-code", "CODE linkify-path", "CODE linkify-code"]);
    assert.equal(spans[0].textContent, "cargo test --lib");
    assert.equal(spans[1].href, "/#/P7?tab=tasks&task=P7.3");
    assert.equal(spans[3].textContent, "see E5 here", "a partial reference inside code stays text");
    const refused = render("`http://127.0.0.1:9/x`").children;
    assert.deepEqual(refused.map(node => node.tagName), ["CODE"]);
  });

  it("previews keep references whole and mark the cut", () => {
    const text = `${"a".repeat(276)} P10.70 tail`;
    const body = render(text, BOARD, { maxCodepoints: 280, copy: false });
    assert.equal(body.textContent, `${"a".repeat(276)} …`);
    assert.equal(anchors(body).length, 0, "a reference crossing the bound is dropped, not split");
    const short = render("see spray.rs:83", BOARD, { maxCodepoints: 280, copy: false });
    assert.equal(short.querySelectorAll("button").length, 0);
    assert.equal(short.textContent, "see spray.rs:83");
    const emoji = render("🛰️".repeat(300), BOARD, { maxCodepoints: 280 }).textContent;
    assert.ok([...emoji].length <= 280 && emoji.endsWith("…") && !emoji.includes("�"));
  });

  it("copies the path command and fails silently", async () => {
    const copied = [];
    navigator.clipboard = { writeText: async text => { copied.push(text); } };
    const button = render("at spray.rs:83").querySelector("button");
    let prevented = false;
    button.listeners.get("click")[0]({ preventDefault() { prevented = true; }, stopPropagation() {} });
    assert.deepEqual(copied, ["trufflepig-agent show path:spray.rs:73-103"]);
    assert.ok(prevented);
    navigator.clipboard = { writeText: () => Promise.reject(new Error("denied")) };
    button.listeners.get("click")[0]({ preventDefault() {} });
    navigator.clipboard = { writeText: () => { throw new Error("blocked"); } };
    button.listeners.get("click")[0]({ preventDefault() {} });
    delete navigator.clipboard;
    button.listeners.get("click")[0]({ preventDefault() {} });
    await new Promise(resolve => setImmediate(resolve));
  });

  it("links text nodes of sanitized markup outside links and code", () => {
    const { dom } = setup();
    const root = dom.el("div"), paragraph = dom.el("p"), anchor = dom.el("a"), code = dom.el("code");
    anchor.append(document.createTextNode("E9 stays"));
    code.append(document.createTextNode("P7.3 stays"));
    paragraph.append(document.createTextNode("Track P7.3 and E5."), anchor, code);
    root.append(paragraph);
    dom.linkifyTextNodes(root, { plan: "P7" });
    assert.deepEqual(anchors(paragraph).map(item => item.href), ["/#/P7?tab=tasks&task=P7.3", "/#/E5", undefined]);
    assert.equal(paragraph.textContent, "Track P7.3 and E5.E9 staysP7.3 stays");
    assert.equal(code.children.length, 0);
  });
});

describe("linkify sinks", () => {
  function views() {
    const { state, dom } = setup(), noop = () => {};
    return createBoardViews({ state, dom, columns: ["todo", "doing", "review", "blocked", "done"],
      collection: (data, key) => data?.[key] || [], entryRecord: value => value?.entry || value,
      permitted: () => false, pageParams: route => route, nextAfter: () => null, queryFilters: () => ({}),
      postKinds: [], entryKinds: [], errorMessage: error => error.message, board: noop, jsonFetch: noop,
      navigate: noop, scheduleRefresh: noop, notice: noop, submitMutation: noop, apiVersion: 1 });
  }

  it("entry cards keep chips only for references the body does not link", () => {
    const entry = { id: "E10", kind: "note", actor: "josh", created_at: 1, plan: "P7",
      body: "Fixed E5 under P7.3 at aad0d2be3786371bf28b742f80f55519c44508b1.",
      refs: ["E5", "P7.3", "AAD0D2BE3786371BF28B742F80F55519C44508B1", "E9"] };
    const card = views().entryCard(entry);
    assert.equal(card.querySelector(".entry-body").textContent, entry.body);
    assert.deepEqual(card.querySelector(".entry-refs").querySelectorAll("a").map(item => item.textContent),
      ["P7", "E9"]);
    assert.deepEqual([...boardTextRefs(entry.body)], ["E5", "P7.3", "aad0d2be3786371bf28b742f80f55519c44508b1"]);
  });

  it("search hits link their target heading and snippet without nesting anchors", () => {
    const page = views().searchPage({ hits: [{ target: "E7", plan: "P7", source: "entry",
      snippet: "blocked on P7.3 ... see E5" }] }, { q: "blocked" });
    const result = page.querySelector(".search-result");
    assert.equal(result.tagName, "ARTICLE");
    assert.equal(result.querySelector(".search-target").href, "/#/E7");
    assert.deepEqual(result.querySelector("p").querySelectorAll("a").map(item => item.href),
      ["/#/P7?tab=tasks&task=P7.3", "/#/E5"]);
    assert.ok(result.querySelectorAll("a").every(item => !item.querySelector("a")));
  });

  it("Working now row scopes stay plain text inside the summary", () => {
    const page = views().renderOverview({ plans: [], working: [{ task: "P7.3", actor: "josh",
      scope: "src/a.rs and E5", last_active: 1 }] }, { entries: [] }, { view: "overview" });
    const scope = page.querySelector(".working-row-scope");
    assert.equal(scope.textContent, "src/a.rs and E5");
    assert.equal(scope.children.length, 0);
  });
});
