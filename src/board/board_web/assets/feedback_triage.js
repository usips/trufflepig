export function createBoardFeedback(context) {
  const { dom, submitMutation, entryRecord, permitted, collection, nextAfter, pageParams, entryCard } = context;
  const { el, add, button, link, badge, empty, omitted, title, field, focusKey, retainForm } = dom;
  function feedbackControls(record) {
    const entry = entryRecord(record);
    const form = el("form");
    const status = record.state || entry.state?.state;
    if (!permitted(record, "can_triage") && !permitted(record, "can_close")) return null;
    const note = field("Note", "note", "", { multiline: true, maxLength: 4096, hint: "optional" });
    const actions = el("div", "actions");
    const control = (label, key, action) => {
      const item = focusKey(button(label, action, "compact"), key);
      item.append(el("span", "sr-only", ` ${entry.id}`));
      return item;
    };
    if (status === "open" && permitted(record, "can_triage")) {
      actions.append(control("Mark triaged", `feedback:${entry.id}:triage`,
        () => void submitMutation(form, { op: "feedback_triage", entry: entry.id, note: note.input.value || null })));
    }
    if (permitted(record, "can_close")) {
      for (const value of ["fixed", "wontfix", "duplicate"]) {
        actions.append(control(`Close: ${value}`, `feedback:${entry.id}:close:${value}`,
          () => void submitMutation(form, {
            op: "feedback_close", entry: entry.id, state: value, note: note.input.value || null,
          })));
      }
    }
    add(form, note.wrapper, actions);
    return retainForm(form, `feedback:${entry.id}`);
  }
  function feedbackMetadata(record) {
    const metadata = record.metadata;
    const details = el("details", "feedback-metadata");
    details.append(el("summary", "small", "Report metadata and recent calls"));
    const facts = el("dl", "metadata-facts");
    for (const [label, value] of [
      ["Version", metadata.version], ["Build", metadata.build_id], ["Repository", metadata.repo_key],
      ["Directory", metadata.cwd], ["Steering", metadata.steer_mode],
    ]) {
      if (value !== null && value !== "") add(facts, el("dt", "small muted", label), el("dd", "small mono", value));
    }
    details.append(facts);
    const calls = el("ol", "recent-calls");
    for (const call of metadata.recent_calls) {
      const item = el("li");
      add(item, el("code", "",
        `${call.verb}${call.args.length ? ` ${call.args.map(argument => JSON.stringify(argument)).join(" ")}` : ""}`),
        el("p", "small muted",
          `Exit ${call.exit_code ?? "unknown"}${call.error_prefix ? ` · ${call.error_prefix}` : ""}`
            + `${call.coverage ? ` · ${call.coverage}` : ""}`),
        call.truncated === true ? badge("truncated") : null);
      calls.append(item);
    }
    details.append(calls.childElementCount ? calls : empty("No recent calls captured."));
    return details;
  }
  function feedbackPage(data, route, entryViews) {
    const page = add(el("div"), title("Feedback", "Reports that improve the board and code search."));
    const tabs = el("nav", "tabs"); tabs.setAttribute("aria-label", "Feedback filters");
    for (const [key, label] of [["open", "Open & triaged"], ["all", "All feedback"]]) {
      tabs.append(link(label, "feedback", { state: key }, (route.state || "open") === key ? "active" : ""));
    }
    page.append(tabs);
    const records = collection(data, "feedback");
    for (const record of records) {
      const card = entryCard(record, { actions: feedbackControls(entryViews.get(record.entry.id)) });
      add(card, badge(record.kind), feedbackMetadata(record)); page.append(card);
    }
    if (!records.length) page.append(empty("No feedback in this view."));
    page.append(omitted(data?.omitted, "feedback reports") || el("span"));
    const cursor = nextAfter(data);
    if (cursor !== null) {
      page.append(link("Next feedback", "feedback",
        { ...pageParams(route, data), state: data.open_only ? "open" : "all" }, "button"));
    }
    return page;
  }

  return { feedbackControls, feedbackMetadata, feedbackPage };
}
