import { createBoardReader } from "/board_reader.js";
import { createBoardStream } from "/board_stream.js";
import { createBoardDom, decodeBoardFragment } from "/board_dom.js";
import { createBoardViews } from "/board_views.js";
import { resolveBootstrapToken } from "/board_web_token.js";

  const TOKEN_KEY = "trufflepig-board-token";
  let stored = "";
  try { stored = sessionStorage.getItem(TOKEN_KEY) || ""; } catch (_) { /* In-memory auth still works. */ }
  const bootstrap = resolveBootstrapToken({
    locationHash: location.hash, locationHref: location.href, storedToken: stored,
  });
  let token = bootstrap.token;
  if (bootstrap.replaceUrl !== null) {
    history.replaceState(history.state, "", bootstrap.replaceUrl);
  }
  if (bootstrap.mismatch) {
    queueMicrotask(() => notice(
      "This link offered a different board token; the tab kept its existing session.", "error"));
  } else if (bootstrap.persist) {
    try { sessionStorage.setItem(TOKEN_KEY, token); } catch (_) { /* In-memory auth still works. */ }
  }
  const apiVersion = Number(document.querySelector('meta[name="board-api"]')?.content);
  // The human's seen mark: the highest event sequence this origin has had
  // delivered, persisted in localStorage; the session baseline renders the
  // "new since you last saw" badge on the overview ticker.
  const SEEN_KEY = "trufflepig-board-seen";
  const seenAtOpen = (() => {
    try {
      const raw = localStorage.getItem(SEEN_KEY);
      return raw && /^(0|[1-9]\d*)$/.test(raw) ? raw : null;
    } catch (_) { return null; }
  })();
  const main = document.getElementById("main");
  const connection = document.getElementById("connection");
  const connectionAnnounce = document.getElementById("connection-announce");
  const freshness = document.getElementById("freshness");
  const state = {
    route: routeFromLocation(), generation: 0, navigation: 0, request: null, loadedAt: 0, clockOffsetMs: 0,
    overview: null, attention: null, watermark: null, stream: null,
    streamGeneration: 0, reconnect: null, streamFailures: 0,
    refreshTimer: null, refreshing: false, refreshAgain: false, drafts: new Map(), globalRefresh: null,
    resyncing: false, liveEntries: [], livePending: new Set(), liveGap: false,
    formDrafts: new Map(), formStatuses: new Map(), pendingForms: new Set(),
    seenAtOpen, seenHigh: seenAtOpen, ingestPending: false,
  };
  const columns = ["todo", "doing", "review", "done", "blocked"];
  const postKinds = ["note", "answer", "decision", "question"];
  const entryKinds = [
    "note", "progress", "review", "question", "answer", "decision", "divergence", "claim", "commit",
    "proposal", "feedback", "task", "hello", "create", "accept", "reject", "direct",
  ];

  const dom = createBoardDom(state);
  const {
    el, add, button, routeUrl, link, refLink, badge, actorName, shortActor, stamp, timeNode, age,
    ageNode, panel, empty, omitted, title, field, formStatus, planId, focusKey,
    captureForms, restoreForms, syncForm, setFormBusy, focusedControl, restoreFocus, saveForm,
  } = dom;
  const views = createBoardViews({
    state, dom, board, jsonFetch, navigate, scheduleRefresh, submitMutation, notice, apiVersion,
    entryRecord, collection, permitted, nextAfter, pageParams, queryFilters, postKinds, entryKinds,
    columns, errorMessage,
  });
  const {
    entryCard, eventList, workingCard, taskCard, attentionItems, attentionCard, renderOverview,
    renderAttention, renderClaims, postForm, taskDetails, planTabs, sanitizedMarkup, renderPlan,
    filtersForm, entriesPage, historyPage, commitsPage, diffPage, feedbackControls, feedbackPage,
    entryPage, searchPage, editorPage,
  } = views;

  const { parseSse, stopStream, startStream } = createBoardStream({
    state, privateFetch, setConnection, loadRoute, scheduleRefresh, parseBoardJson, addTickerEvent,
    addLiveEntry, noteSeen, onIngest,
  });

  const { fetchRoute } = createBoardReader({
    state, dom, views, board, jsonFetch, apiVersion, readOp, snapshotSeq, minimumSeq, cursorFromRoute,
    parseBoardJson, queryFilters,
  });

  function noteSeen(seq) {
    const text = String(seq);
    if (!/^(0|[1-9]\d*)$/.test(text)) return;
    if (state.seenHigh !== null && BigInt(text) <= BigInt(state.seenHigh)) return;
    state.seenHigh = text;
    try { localStorage.setItem(SEEN_KEY, text); } catch (_) { /* The mark is a convenience, not a cursor. */ }
  }
  function onIngest(result) {
    state.ingestPending = false;
    if (result?.error) notice(`Repository ingestion failed: ${result.error.message || "unknown error"}`, "error");
    else {
      const counts = ["scanned", "inserted", "skipped"]
        .map(key => Number.isFinite(result?.[key]) ? `${result[key]} ${key}` : null)
        .filter(Boolean).join(", ");
      notice(`Repository ingestion completed${counts ? `: ${counts}` : "."}`, "success");
    }
    scheduleRefresh();
  }

  function notice(message, tone = "") {
    const item = document.getElementById("notice");
    item.textContent = message;
    item.dataset.tone = tone;
    item.hidden = !message;
  }
  // The indicator itself stays out of the live region: only losing an
  // established connection and recovering from an outage are announced,
  // never routine (re)connect ticks or follower-mode "Live" updates.
  let connectionAnnounced = "";
  function setConnection(label, mode) {
    connection.textContent = label; connection.dataset.state = mode;
    if (mode === connectionAnnounced) return;
    if (mode === "error" && connectionAnnounced === "live") {
      connectionAnnounce.textContent = "Board connection lost. Reconnecting.";
    } else if (mode === "live" && connectionAnnounced === "error") {
      connectionAnnounce.textContent = "Board connection restored.";
    }
    connectionAnnounced = mode;
  }
  function routeFromLocation() {
    const [path = "", queryText = ""] = (location.hash.startsWith("#/") ? location.hash.slice(2) : "").split("?");
    const query = new URLSearchParams(queryText);
    const route = Object.fromEntries([
      "tab", "q", "plan", "kind", "harness", "user", "host", "task", "after", "through", "state",
      "heading", "oid", "repo", "taskAfter", "taskCeiling", "claimAfter", "repliesAfter",
      "backrefsAfter", "own_stale", "taskThrough", "claimThrough", "repliesThrough",
      "backrefsThrough",
    ].map(key => [key, query.get(key) || ""]));
    const target = decodeBoardFragment(path);
    route.view = /^P[1-9]\d*/.test(target) ? "plan"
      : /^E[1-9]\d*$/.test(target) ? "entry"
      : target.startsWith("edit/") ? "edit" : target || "overview";
    route.ref = ["plan", "entry"].includes(route.view) ? target : route.view === "edit" ? target.slice(5) : "";
    return route;
  }
  function navigate(view, params = {}, replace = false) {
    history[replace ? "replaceState" : "pushState"]({}, "", routeUrl(view, params));
    state.navigation++;
    state.route = routeFromLocation();
    notice("");
    void loadRoute(true);
  }
  function errorMessage(error) { return error?.message || String(error); }
  function isAbort(error) { return error?.name === "AbortError"; }
  function assertAuth() {
    if (!token) throw new Error("Open the launch URL printed by trufflepig board web to authorize this tab.");
    if (!Number.isInteger(apiVersion) || apiVersion < 1) {
      throw new Error("The board API version is missing from the server shell.");
    }
  }
  async function privateFetch(url, options = {}) {
    assertAuth();
    const headers = new Headers(options.headers);
    headers.set("X-Board-Token", token);
    return fetch(url, { ...options, headers, cache: "no-store", credentials: "omit", referrerPolicy: "no-referrer" });
  }
  async function jsonFetch(url, options = {}) {
    const response = await privateFetch(url, options);
    let body;
    try { body = parseBoardJson(await response.text()); }
    catch (_) { throw new Error(`Board returned an unreadable response (${response.status}).`); }
    if (!response.ok || body.error) {
      const detail = body.error || body;
      const failure = new Error(detail.message || `Board request failed (${response.status}).`);
      failure.code = detail.code || "";
      failure.status = response.status;
      if (response.status === 401 || response.status === 403) setConnection("Authorization required", "error");
      throw failure;
    }
    return body;
  }
  function parseBoardJson(text) {
    // SQLite sequences may exceed JavaScript's exact integer range.
    const exact = text.replace(
      /("(?:[^"\\]|\\.)*")|(-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/g,
      (token, string, number) => {
        return string || !/^\d+$/.test(number) || Number.isSafeInteger(Number(number))
          ? token : JSON.stringify(number);
      }
    );
    return JSON.parse(exact);
  }
  function encodeBoardJson(value, key = "") {
    if (["seq", "after", "through", "claim", "ordinal"].includes(key) && typeof value === "string"
      && /^(0|[1-9]\d*)$/.test(value)) return value;
    if (Array.isArray(value)) return `[${value.map(item => encodeBoardJson(item)).join(",")}]`;
    if (value && typeof value === "object") {
      const fields = Object.entries(value)
        .map(([name, item]) => `${JSON.stringify(name)}:${encodeBoardJson(item, name)}`);
      return `{${fields.join(",")}}`;
    }
    return JSON.stringify(value);
  }
  async function board(op, signal) {
    const reply = await jsonFetch("/api/v1/board", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: encodeBoardJson({ api: apiVersion, op }), signal,
    });
    if (reply.api !== apiVersion || !reply.result || !Object.hasOwn(reply.result, "data")) {
      throw new Error("Board returned an incompatible reply.");
    }
    return {
      type: reply.result.result, data: reply.result.data,
      seq: reply.snapshot_seq, warnings: reply.warnings || [],
    };
  }
  function readOp(op, fields = {}) { return { op, ...fields }; }
  function snapshotSeq(reply) {
    const seq = reply?.seq;
    return seq !== undefined && seq !== null && /^(0|[1-9]\d*)$/.test(String(seq))
      && BigInt(seq) <= 9223372036854775807n ? String(seq) : null;
  }
  function minimumSeq(replies) {
    const values = replies.map(snapshotSeq);
    if (values.some(value => value === null)) {
      throw new Error("A board snapshot is missing its sequence. Refresh to retry.");
    }
    return values.length ? values.reduce((a, b) => BigInt(a) < BigInt(b) ? a : b) : null;
  }
  function entryRecord(record) { return record?.entry && typeof record.entry === "object" ? record.entry : record; }
  function collection(data, key) {
    if (!Array.isArray(data?.[key])) throw new Error(`Board reply is missing its ${key} collection.`);
    return data[key];
  }
  function permitted(data, name) { return data?.[name] === true; }
  function nextAfter(data) { return data?.next_after ?? null; }
  function cursorFromRoute(route, composite = false) {
    if (!route.after) return null;
    if (composite) return parseBoardJson(route.after);
    if (!/^(0|[1-9]\d*)$/.test(route.after)) throw new Error("Invalid page cursor.");
    return route.after;
  }
  function pageParams(route, data) {
    const cursor = nextAfter(data);
    return { ...route, after: typeof cursor === "object" ? JSON.stringify(cursor) : cursor, through: data.through };
  }
  function queryFilters(route) {
    return {
      plan: route.plan || null, kind: route.kind || null, harness: route.harness || null,
      user: route.user || null, host: route.host || null, task: route.task || null,
      references: null, after: cursorFromRoute(route, true), through: route.through || null,
      limit: 50,
    };
  }
  function scheduleRefresh() {
    if (state.refreshTimer !== null) return;
    state.refreshTimer = setTimeout(() => { state.refreshTimer = null; void loadRoute(false); }, 150);
  }
  function addTickerEvent(event) {
    if (!state.overview || !event?.seq) return;
    const events = state.overview.events || (state.overview.events = []);
    if (!events.some(item => String(item.seq) === String(event.seq))) events.unshift(event);
    if (events.length > 20) events.length = 20;
  }
  function addLiveEntry(event) {
    const route = state.route, subject = String(event?.subject);
    if (!/^E[1-9]\d*$/.test(subject) || route.after || route.through) return;
    const plan = route.view === "entries" ? route.plan
      : route.view === "plan" && route.tab === "entries" ? planId(route.ref) : null;
    if (plan === null) return;
    if (plan && String(event.plan || "") !== plan) return;
    if (route.kind && event.kind !== route.kind) return;
    if (state.livePending.has(subject) || state.liveEntries.some(item => item.entry.id === subject)) return;
    state.livePending.add(subject);
    void (async () => {
      try {
        const entry = entryRecord((await board(readOp("show", { target: subject }))).data.entry);
        if (!entry?.id || state.liveEntries.some(item => item.entry.id === entry.id)) return;
        if (String(entry.seq) !== String(event.seq)) return;
        const record = { seq: String(event.seq), entry };
        const at = state.liveEntries.findIndex(item =>
          BigInt(item.seq) > BigInt(record.seq)
            || (BigInt(item.seq) === BigInt(record.seq)
              && BigInt(item.entry.id.slice(1)) > BigInt(record.entry.id.slice(1))));
        state.liveEntries.splice(at < 0 ? state.liveEntries.length : at, 0, record);
        if (state.liveEntries.length > 50) {
          const oldest = state.liveEntries.reduce(
            (lowest, item, index) => BigInt(item.seq) < BigInt(state.liveEntries[lowest].seq) ? index : lowest,
            0
          );
          state.liveEntries.splice(oldest, 1);
          state.liveGap = true;
        }
        scheduleRefresh();
      } catch (_) { /* A failed Show fetch is recovered by the next refresh. */ }
      finally { state.livePending.delete(subject); }
    })();
  }
  async function submitMutation(form, op, success) {
    const navigation = state.navigation;
    const key = form.dataset.draftKey;
    saveForm(form); if (key) state.pendingForms.add(key); setFormBusy(form, true);
    formStatus(form, "Saving…", "muted");
    try {
      const result = await board(op);
      const current = navigation === state.navigation;
      if (key) { state.formDrafts.delete(key); state.formStatuses.delete(key); }
      formStatus(form, current ? "Saved." : "", "muted");
      if (success) success(result, current);
      else if (current) { notice("Saved.", "success"); scheduleRefresh(); }
      if (key) { saveForm(form); syncForm(form, main); if (!current) state.formStatuses.delete(key); }
      return true;
    } catch (error) {
      const stale = error.code === "stale_revision" || errorMessage(error).startsWith("stale_revision");
      formStatus(form, stale
        ? `The plan changed. Your draft and its original base are preserved. ${errorMessage(error)}`
        : errorMessage(error));
      return false;
    } finally {
      if (key) state.pendingForms.delete(key); setFormBusy(form, false);
      for (const current of main.querySelectorAll("form"))
        if (current.dataset.draftKey === key) setFormBusy(current, false);
    }
  }
  async function loadRoute(focus = false, forceSnapshot = false) {
    forceSnapshot ||= state.resyncing;
    if (state.refreshing && !focus && !forceSnapshot) { state.refreshAgain = true; return; }
    state.request?.abort();
    const controller = new AbortController(); state.request = controller;
    const generation = ++state.generation;
    const route = { ...state.route };
    captureForms(main);
    state.refreshing = true; state.refreshAgain = false;
    main.setAttribute("aria-busy", "true");
    if (focus) main.replaceChildren(el("div", "loading", "Loading…"));
    document.querySelectorAll("[data-route]")
      .forEach(item => item.classList.toggle("active", item.dataset.route === route.view));
    try {
      assertAuth();
      const result = await fetchRoute(route, controller.signal, forceSnapshot);
      if (generation !== state.generation) return;
      if (result.overview) state.overview = result.overview;
      if (result.attention) state.attention = result.attention;
      if (result.serverNow !== null) state.clockOffsetMs = result.serverNow * 1000 - Date.now();
      const active = focus ? null : focusedControl(main);
      captureForms(main); restoreForms(result.page); main.replaceChildren(result.page); restoreFocus(main, active);
      state.loadedAt = Date.now();
      if (result.watermark !== null) noteSeen(result.watermark);
      const count = attentionItems(state.attention).length;
      const label = document.getElementById("attention-count"); label.textContent = count; label.hidden = !count;
      if (result.warnings.length) notice(result.warnings.join(" · "));
      if (forceSnapshot || state.watermark === null) {
        state.watermark = result.watermark;
        state.resyncing = false;
        if (state.watermark !== null) startStream(state.watermark);
      } else if (!state.stream && !state.reconnect) startStream(state.watermark);
      if (focus) {
        main.focus({ preventScroll: true }); window.scrollTo({ top: 0 });
        const anchor = route.heading || (route.oid ? `commit-${route.oid}` : route.task);
        if (anchor) document.getElementById(anchor)?.scrollIntoView?.({ block: "start" });
      }
    } catch (error) {
      if (isAbort(error) || generation !== state.generation) return;
      if (!main.querySelector("form") || focus) {
        const box = panel("Board unavailable", add(el("div"), el("p", "", errorMessage(error)),
          focusKey(button("Try again", () => void loadRoute(true)), "try-again")), "callout");
        main.replaceChildren(box);
      } else notice(errorMessage(error), "error");
      setConnection(token ? "Request failed" : "Authorization required", "error");
    } finally {
      if (generation === state.generation) {
        state.refreshing = false; main.setAttribute("aria-busy", "false");
        if (state.refreshAgain) scheduleRefresh();
      }
    }
  }
  async function refreshOverview() {
    if (!token || state.globalRefresh) return;
    const controller = new AbortController(); state.globalRefresh = controller;
    try {
      const overview = await board(
        readOp("overview", { repo_key: null, after: null, through: null, limit: 50 }),
        controller.signal
      );
      if (snapshotSeq(overview) === null) throw new Error("Overview is missing its snapshot sequence.");
      state.overview = { ...overview.data, events: state.overview?.events || [] };
      if (Number.isFinite(overview.data.server_now)) state.clockOffsetMs = overview.data.server_now * 1000 - Date.now();
    } catch (error) { if (!isAbort(error)) setConnection("Refresh failed", "error"); }
    finally { if (state.globalRefresh === controller) state.globalRefresh = null; }
  }
  document.addEventListener("click", event => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey
      || event.altKey) return;
    const fragmentLink = event.target.closest("a[href^='#']");
    if (fragmentLink && !fragmentLink.getAttribute("href").startsWith("#/")) {
      event.preventDefault();
      const heading = decodeBoardFragment(fragmentLink.getAttribute("href").slice(1));
      history.replaceState({}, "", routeUrl(state.route.view, { ...state.route, heading }));
      state.route.heading = heading; document.getElementById(heading)?.scrollIntoView?.({ block: "start" });
      if (heading === "main") main.focus({ preventScroll: true }); return;
    }
    const anchor = event.target.closest("a[data-nav],a[data-route]");
    if (!anchor || event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey
      || event.shiftKey || event.altKey) return;
    event.preventDefault();
    const target = new URL(anchor.href, location.href);
    history.pushState({}, "", target.pathname + target.search + target.hash);
    const next = routeFromLocation();
    if (routeUrl(next.view, { ...next, heading: "" }) === routeUrl(state.route.view, { ...state.route, heading: "" })) {
      state.route = next; document.getElementById(next.heading)?.scrollIntoView?.({ block: "start" }); return;
    }
    state.navigation++;
    state.route = routeFromLocation(); notice(""); void loadRoute(true);
  });
  window.addEventListener("hashchange", () => {
    state.navigation++; state.route = routeFromLocation(); void loadRoute(true);
  });
  window.addEventListener("pagehide", () => { stopStream(); state.request?.abort(); state.globalRefresh?.abort(); });
  window.addEventListener("pageshow", event => { if (event.persisted) void loadRoute(false, true); });
  document.getElementById("refresh").addEventListener("click", () => void loadRoute(false));
  setInterval(() => {
    for (const item of document.querySelectorAll("[data-age]")) item.textContent = age(item.dataset.age);
    freshness.textContent = state.loadedAt ? `Updated ${Math.floor((Date.now() - state.loadedAt) / 1000)}s ago` : "";
  }, 1000);
  setInterval(() => {
    if (document.visibilityState !== "hidden") {
      if (state.route.view !== "overview") void refreshOverview();
      scheduleRefresh();
    }
  }, 15000);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") scheduleRefresh();
  });
  void loadRoute(true);
