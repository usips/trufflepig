import { resyncSeenMark } from "./board_seen.js";

export function createBoardRenderLoop({
  state, dom, main, apiVersion, seenStorage, seenKey,
  getToken, isExpired, enterExpired, notice, setConnection, noteSeen, wiring,
}) {
  const {
    el, add, button, panel, focusKey,
    captureForms, restoreForms, syncForm, setFormBusy, formStatus, focusedControl, restoreFocus,
    saveForm, markFormSaved,
  } = dom;

  function errorMessage(error) { return error?.message || String(error); }
  function isAbort(error) { return error?.name === "AbortError"; }
  function assertAuth() {
    if (!getToken()) throw new Error("Open the launch URL printed by trufflepig board web to authorize this tab.");
    if (!Number.isInteger(apiVersion) || apiVersion < 1) {
      throw new Error("The board API version is missing from the server shell.");
    }
  }
  async function privateFetch(url, options = {}) {
    assertAuth();
    const headers = new Headers(options.headers);
    headers.set("X-Board-Token", getToken());
    return fetch(url, { ...options, headers, cache: "no-store", credentials: "omit", referrerPolicy: "no-referrer" });
  }
  async function jsonFetch(url, options = {}) {
    const sent = getToken();
    const response = await privateFetch(url, options);
    let body;
    try { body = parseBoardJson(await response.text()); }
    catch (_) { throw new Error(`Board returned an unreadable response (${response.status}).`); }
    if (!response.ok || body.error) {
      const detail = body.error || body;
      const failure = new Error(detail.message || `Board request failed (${response.status}).`);
      failure.code = detail.code || "";
      failure.status = response.status;
      if (response.status === 401 || response.status === 403) enterExpired(sent);
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
  // Entries pages continue with next_before; every other collection uses next_after.
  function nextAfter(data) { return data?.next_after ?? data?.next_before ?? null; }
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
      references: null, before: cursorFromRoute(route, true), through: route.through || null,
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
  async function submitMutation(form, op, success) {
    const navigation = state.navigation;
    const key = form.dataset.draftKey;
    const editor = key?.startsWith("editor:");
    saveForm(form); if (key) state.pendingForms.add(key); setFormBusy(form, true);
    formStatus(form, "Saving…", "muted");
    try {
      const result = await board(op);
      const current = navigation === state.navigation;
      if (key) { state.formDrafts.delete(key); state.formStatuses.delete(key); }
      if (editor) markFormSaved(form, main);
      formStatus(form, current ? "Saved." : "", "muted", !editor);
      if (success) success(result, current);
      else if (current) { notice("Saved.", "success"); scheduleRefresh(); }
      if (key) {
        if (!editor) saveForm(form);
        syncForm(form, main);
        if (!current) state.formStatuses.delete(key);
      }
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
      const result = await wiring.fetchRoute(route, controller.signal, forceSnapshot);
      if (generation !== state.generation) return;
      if (result.overview) state.overview = result.overview;
      if (result.attention) state.attention = result.attention;
      if (result.serverNow !== null) state.clockOffsetMs = result.serverNow * 1000 - Date.now();
      const active = focus ? null : focusedControl(main);
      captureForms(main); restoreForms(result.page); main.replaceChildren(result.page); restoreFocus(main, active);
      state.loadedAt = Date.now();
      if (result.watermark !== null) {
        // A fresh baseline is the only legitimate lowering: the stored mark
        // can exceed it only when the database was replaced under the tab.
        if (forceSnapshot || state.watermark === null) {
          const mark = resyncSeenMark(seenStorage, seenKey, result.watermark);
          if (mark !== null) state.seenHigh = mark;
        }
        noteSeen(result.watermark);
      }
      const count = wiring.attentionItems(state.attention).length;
      const label = document.getElementById("attention-count"); label.textContent = count; label.hidden = !count;
      if (result.warnings.length) notice(result.warnings.join(" · "));
      if (forceSnapshot || state.watermark === null) {
        state.watermark = result.watermark;
        state.resyncing = false;
        if (state.watermark !== null) wiring.startStream(state.watermark);
      } else if (!state.stream && !state.reconnect) wiring.startStream(state.watermark);
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
      if (!isExpired()) setConnection(getToken() ? "Request failed" : "Authorization required", "error");
    } finally {
      if (generation === state.generation) {
        state.refreshing = false; main.setAttribute("aria-busy", "false");
        if (state.refreshAgain) scheduleRefresh();
      }
    }
  }
  async function refreshOverview() {
    if (!getToken() || state.globalRefresh) return;
    const controller = new AbortController(); state.globalRefresh = controller;
    try {
      const overview = await board(
        readOp("overview", { repo_key: null, after: null, through: null, limit: 50 }),
        controller.signal
      );
      if (snapshotSeq(overview) === null) throw new Error("Overview is missing its snapshot sequence.");
      state.overview = { ...overview.data, events: state.overview?.events || [] };
      if (Number.isFinite(overview.data.server_now)) state.clockOffsetMs = overview.data.server_now * 1000 - Date.now();
    } catch (error) { if (!isAbort(error) && !isExpired()) setConnection("Refresh failed", "error"); }
    finally { if (state.globalRefresh === controller) state.globalRefresh = null; }
  }

  return {
    board, jsonFetch, privateFetch, parseBoardJson, readOp, snapshotSeq, minimumSeq,
    entryRecord, collection, permitted, nextAfter, cursorFromRoute, pageParams, queryFilters,
    errorMessage, scheduleRefresh, submitMutation, loadRoute, refreshOverview, addTickerEvent,
  };
}
