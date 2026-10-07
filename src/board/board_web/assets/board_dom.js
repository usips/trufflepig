export function decodeBoardFragment(value) {
  try { return decodeURIComponent(value); } catch (_) { return value; }
}

export function createBoardDom(state) {
  let fieldSequence = 0;
  const busyControls = new WeakMap();
  const editedForms = new WeakSet(), savedForms = new WeakSet();
  function el(tag, className, text) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined && text !== null) element.textContent = String(text);
    return element;
  }
  function add(parent, ...children) {
    for (const child of children.flat()) if (child !== null && child !== undefined) parent.append(child);
    return parent;
  }
  function button(label, action, className = "") {
    const item = el("button", className, label);
    item.type = "button";
    if (action) item.addEventListener("click", action);
    return item;
  }
  function routeUrl(view, params = {}) {
    const path = view === "overview" ? ""
      : ["plan", "entry"].includes(view) ? params.ref
      : view === "edit" ? `edit/${params.ref}` : view;
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(params)) {
      if (!["view", "ref"].includes(key) && value !== undefined && value !== null && value !== "") {
        query.set(key, value);
      }
    }
    return `/#/${path || ""}${query.size ? `?${query}` : ""}`;
  }
  function link(label, view, params = {}, className = "") {
    const item = el("a", className, label);
    item.href = routeUrl(view, params);
    item.dataset.nav = "true";
    item.dataset.focusKey = `link:${item.href}`;
    return item;
  }
  // A deterministic key lets focus survive a wholesale page swap. Keyed cards
  // and sections take tabindex="-1" so restore targets take programmatic
  // focus in a real browser; links and controls are focusable already.
  function focusKey(item, key) {
    item.dataset.focusKey = key;
    const tag = item.tagName;
    const native = tag === "BUTTON" || tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA"
      || (tag === "A" && item.href !== undefined && item.href !== null && item.href !== "");
    if (!native) item.setAttribute("tabindex", "-1");
    return item;
  }
  function refLink(ref, label = ref, className = "mono", context = {}) {
    if (!ref) return el("span", className, label || "—");
    const text = String(ref);
    if (/^E[1-9]\d*$/.test(text)) return link(label, "entry", { ref: text }, className);
    if (/^P[1-9]\d*\.\d+$/.test(text)) {
      return link(label, "plan", { ref: planId(text), tab: "tasks", task: text }, className);
    }
    if (/^P[1-9]\d*(?:@\d+(?:\.\.(?:\d+)?)?)?$/.test(text)) return link(label, "plan", { ref: text }, className);
    if (/^(?:[a-fA-F0-9]{40}|[a-fA-F0-9]{64})$/.test(text)) {
      return context.plan
        ? link(label, "plan",
          { ref: context.plan, tab: "commits", oid: text.toLowerCase(), repo: context.repo_key }, className)
        : link(label, "search", { q: text }, className);
    }
    return el("span", className, label);
  }
  function badge(value) {
    const item = el("span", "badge", value || "unknown");
    item.dataset.kind = value || "unknown";
    return item;
  }
  function actorName(actor) {
    if (!actor) return "Unknown actor";
    if (typeof actor === "string") return actor;
    return `${actor.user}@${actor.host}/${actor.harness}/${actor.session}`;
  }
  function shortActor(actor) { return typeof actor === "object" && actor ? actor.harness : actorName(actor); }
  function stamp(value) {
    const date = new Date(Number(value) * 1000);
    return Number.isFinite(date.getTime()) ? date.toLocaleString() : "Unknown time";
  }
  function timeNode(value) {
    const item = el("time", "", stamp(value));
    const date = new Date(Number(value) * 1000);
    if (Number.isFinite(date.getTime())) item.dateTime = date.toISOString();
    return item;
  }
  function age(value) {
    const seconds = Math.max(0, Math.floor((Date.now() + state.clockOffsetMs) / 1000 - Number(value)));
    if (!Number.isFinite(seconds)) return "unknown";
    if (seconds < 60) return `${seconds}s ago`;
    if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
    if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
    return `${Math.floor(seconds / 86400)}d ago`;
  }
  function ageNode(value) {
    const item = el("span", "", age(value));
    item.dataset.age = value;
    item.title = stamp(value);
    return item;
  }
  function panel(title, body, className = "") {
    const item = el("section", `panel section ${className}`);
    if (title) item.append(el("h2", "", title));
    if (body) item.append(body);
    return focusKey(item, `section:${title}`);
  }
  function empty(text) { return el("p", "empty", text); }
  function omitted(value, label = "records") {
    return Number(value) > 0 ? el("p", "omitted", `${value} more ${label} omitted from this snapshot.`) : null;
  }
  function title(heading, detail, actions) {
    const item = el("div", "page-title");
    add(item, add(el("div"), el("h1", "", heading), detail ? el("p", "", detail) : null), actions);
    return item;
  }
  function field(label, name, value = "", options = {}) {
    const wrapper = el("div", "field");
    const id = `field-${name}-${++fieldSequence}`;
    const caption = el("label", "", label); caption.htmlFor = id;
    const input = options.choices ? el("select") : el(options.multiline ? "textarea" : "input");
    input.id = id; input.name = name;
    if (options.choices) for (const choice of options.choices) {
      const [key, text] = Array.isArray(choice) ? choice : [choice, choice];
      const option = el("option", "", text); option.value = key; input.append(option);
    }
    if (options.type) input.type = options.type;
    input.value = value;
    if (options.required) input.required = true;
    if (options.maxLength) input.maxLength = options.maxLength;
    if (options.className) input.className = options.className;
    if (options.placeholder) input.placeholder = options.placeholder;
    let hint = null;
    if (options.hint) {
      hint = el("span", "hint", options.hint);
      hint.id = `${id}-hint`; input.setAttribute("aria-describedby", hint.id);
    }
    add(wrapper, caption, input, hint);
    return { wrapper, input };
  }
  function formStatus(form, message, tone = "error", retain = true) {
    if (retain && form.dataset.draftKey) state.formStatuses.set(form.dataset.draftKey, { message, tone });
    const forms = [form, ...document.querySelectorAll("form")].filter((item, index, all) =>
      all.indexOf(item) === index
        && (item === form || form.dataset.draftKey && item.dataset.draftKey === form.dataset.draftKey));
    for (const target of forms) {
      let item = target.querySelector(".form-status");
      if (!item) { item = el("p", "form-status"); item.setAttribute("role", "status"); target.append(item); }
      item.textContent = message; item.className = `form-status ${tone}`;
    }
  }
  // Editors enter the stores on input; saved, empty and unchanged forms
  // stay out when refreshes or navigation capture the mounted controls.
  function saveForm(form) {
    const key = form.dataset.draftKey;
    if (!key || savedForms.has(form)) return;
    if (key.startsWith("editor:") && !editedForms.has(form)) return;
    const inputs = [...form.querySelectorAll("input,textarea,select")]
      .filter(input => input.name && !input.readOnly)
      .map(input => [input.name, input.value]);
    if (!inputs.length) return;
    const prior = state.formDrafts.get(key);
    if (prior && Object.keys(prior).length === inputs.length
      && inputs.every(([name, value]) => prior[name] === value)) return;
    state.formDrafts.set(key, Object.fromEntries(inputs));
  }
  function restoreForm(form) {
    const draft = state.formDrafts.get(form.dataset.draftKey);
    if (draft) {
      editedForms.add(form);
      for (const input of form.querySelectorAll("input,textarea,select")) {
        if (!input.readOnly && Object.hasOwn(draft, input.name)) input.value = draft[input.name];
      }
    }
    const status = state.formStatuses.get(form.dataset.draftKey);
    if (status) formStatus(form, status.message, status.tone);
    if (state.pendingForms.has(form.dataset.draftKey)) setFormBusy(form, true);
  }
  function retainForm(form, key) {
    form.dataset.draftKey = key;
    restoreForm(form);
    const changed = () => { savedForms.delete(form); editedForms.add(form); saveForm(form); };
    form.addEventListener("input", changed);
    form.addEventListener("change", changed);
    return form;
  }
  function markFormSaved(form, root) {
    for (const current of [form, ...root.querySelectorAll("form")]) {
      if (current === form || form.dataset.draftKey && current.dataset.draftKey === form.dataset.draftKey) {
        savedForms.add(current); editedForms.delete(current);
      }
    }
  }
  function captureForms(root) { for (const form of root.querySelectorAll("form")) saveForm(form); }
  function restoreForms(root) { for (const form of root.querySelectorAll("form")) restoreForm(form); }
  function syncForm(form, root) {
    const values = new Map([...form.querySelectorAll("input,textarea,select")].map(input => [input.name, input.value]));
    for (const current of root.querySelectorAll("form")) if (current.dataset.draftKey === form.dataset.draftKey) {
      if (savedForms.has(form)) { savedForms.add(current); editedForms.delete(current); }
      for (const input of current.querySelectorAll("input,textarea,select")) {
        if (values.has(input.name)) input.value = values.get(input.name);
      }
    }
  }
  function setFormBusy(form, busy) {
    if (busy && !busyControls.has(form)) {
      const controls = [...form.querySelectorAll("button,input,textarea,select")].map(input => [input, input.disabled]);
      busyControls.set(form, controls); controls.forEach(([input]) => { input.disabled = true; });
    } else if (!busy && busyControls.has(form)) {
      busyControls.get(form).forEach(([input, disabled]) => { input.disabled = disabled; }); busyControls.delete(form);
    }
  }
  // Duplicate hrefs share one key, so the recorded key carries the element's
  // occurrence among same-key matches, counted lazily here because pages build
  // incrementally. The occurrence is scoped within the nearest keyed ancestor,
  // so a new row elsewhere (a fresh ticker event) never shifts other keys.
  // The ancestor and its own occurrence ride along as fallbacks for when the
  // element itself is gone after a refresh, and to scope duplicates of the
  // ancestor (two panels with the same key) to the instance that held focus.
  function focusedControl(root) {
    const input = document.activeElement;
    if (!root.contains(input)) return null;
    const form = input?.closest?.("form");
    if (form?.dataset.draftKey && input.name) {
      return { key: form.dataset.draftKey, name: input.name, start: input.selectionStart, end: input.selectionEnd };
    }
    const keyed = input?.closest?.("[data-focus-key]");
    if (!keyed) return null;
    const key = keyed.dataset.focusKey;
    const ancestor = keyed.parentElement?.closest?.("[data-focus-key]");
    const scope = ancestor && root.contains(ancestor) ? ancestor : root;
    const occurrence = [...scope.querySelectorAll(`[data-focus-key="${CSS.escape(key)}"]`)].indexOf(keyed);
    const ancestorKey = ancestor ? ancestor.dataset.focusKey : null;
    const ancestorOccurrence = ancestor
      ? [...root.querySelectorAll(`[data-focus-key="${CSS.escape(ancestorKey)}"]`)].indexOf(ancestor)
      : 0;
    return {
      focusKey: `${key}#${occurrence}`,
      ancestorKey: ancestor ? `${ancestorKey}#${ancestorOccurrence}` : null,
      formKey: form?.dataset.draftKey || null,
    };
  }
  function restoreFocus(root, focus) {
    if (!focus) return;
    if (focus.focusKey) {
      const keyed = /^(.*)#(\d+)$/.exec(focus.focusKey);
      const key = keyed ? keyed[1] : focus.focusKey;
      const occurrence = keyed ? Number(keyed[2]) : 0;
      const recorded = focus.ancestorKey ? /^(.*)#(\d+)$/.exec(focus.ancestorKey) : null;
      const ancestorKey = recorded ? recorded[1] : focus.ancestorKey;
      const ancestorOccurrence = recorded ? Number(recorded[2]) : 0;
      const ancestors = ancestorKey
        ? [...root.querySelectorAll(`[data-focus-key="${CSS.escape(ancestorKey)}"]`)] : [];
      const scope = ancestors[ancestorOccurrence] || ancestors[0] || null;
      const matches = [...(scope || root).querySelectorAll(`[data-focus-key="${CSS.escape(key)}"]`)];
      const target = matches[occurrence] || scope || null;
      if (target) { target.focus({ preventScroll: true }); return; }
      if (focus.formKey) {
        const form = [...root.querySelectorAll("form")].find(item => item.dataset.draftKey === focus.formKey);
        const control = [...(form?.querySelectorAll("input,textarea,select,button,a") || [])]
          .find(item => !item.disabled && (item.tagName !== "A" || item.href));
        if (control) control.focus({ preventScroll: true });
      }
      return;
    }
    const form = [...root.querySelectorAll("form")].find(item => item.dataset.draftKey === focus.key);
    const input = [...(form?.querySelectorAll("input,textarea,select") || [])].find(item => item.name === focus.name);
    if (input) {
      input.focus({ preventScroll: true });
      if (focus.start !== null && focus.start !== undefined && input.setSelectionRange) {
        input.setSelectionRange(focus.start, focus.end);
      }
    }
  }
  function planId(ref) { return String(ref || "").match(/^P[1-9]\d*/)?.[0] || ""; }

  return {
    el, add, button, routeUrl, link, refLink, badge, actorName, shortActor, stamp, timeNode, age,
    ageNode, panel, empty, omitted, title, field, formStatus, planId, focusKey,
    retainForm, captureForms, restoreForms, syncForm, setFormBusy, focusedControl, restoreFocus, saveForm, markFormSaved,
  };
}
