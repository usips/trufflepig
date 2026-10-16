const AUTHORITY_ROLES = ["triage", "release", "link", "assign", "decide"];
const GRANT_DURATIONS = [
  ["3600", "1 hour"], ["86400", "1 day"], ["604800", "7 days"], ["2592000", "30 days"],
  ["", "No expiry"],
];
const INVALID_IDENTITY_CHARS = /[\p{White_Space}\p{Cc}/@]/u;
const INVALID_HARNESS_CHARS = /[\p{White_Space}\p{Cc}/]/u;

function byteLength(value) {
  return new TextEncoder().encode(value).length;
}

function validActorComponent(value) {
  return byteLength(value) > 0 && byteLength(value) <= 256 && !INVALID_IDENTITY_CHARS.test(value);
}

function validHarness(value) {
  if (byteLength(value) === 0 || byteLength(value) > 256 || INVALID_HARNESS_CHARS.test(value)) return false;
  if (value.startsWith("git:")) return value.length > 4;
  return /^[a-z0-9_-]+$/.test(value);
}

function validGrantee(value, scope) {
  const parts = value.split("/");
  if (scope === "harness") return parts.length === 1 && validHarness(value);
  if (scope !== "session") return false;
  if (parts.length === 2) return validHarness(parts[0]) && validActorComponent(parts[1]);
  if (parts.length !== 3 || !validHarness(parts[1]) || !validActorComponent(parts[2])) return false;
  const userHost = parts[0].split("@");
  return userHost.length === 2 && validActorComponent(userHost[0]) && validActorComponent(userHost[1]);
}

export function createBoardGrantPanel(context) {
  const { state, dom, submitMutation, scheduleRefresh, notice } = context;
  const { el, add, badge, button, field, focusKey, formStatus, panel, refLink, timeNode, retainForm } = dom;

  function grantState(grant, now) {
    if (grant.revoked_by) return "revoked";
    if (grant.expires_at !== null && grant.expires_at !== undefined && Number(grant.expires_at) <= now) {
      return "expired";
    }
    return "live";
  }

  function grantRow(grant, canManage, now) {
    const status = grantState(grant, now);
    const item = el("article", "grant-row"); item.dataset.status = status;
    const roles = add(el("div", "grant-roles"), grant.roles.map(role => badge(role)));
    const state = add(el("div", "grant-heading"), refLink(grant.entry, `Grant ${grant.entry}`), badge(status),
      el("span", "mono grant-grantee", String(grant.grantee)));
    add(item, state, roles,
      el("p", "grant-scope small muted", `Owner scope: ${grant.owner_user}`),
      add(el("p", "grant-source small muted"), el("span", "", `Source ${grant.source} · granted `),
        timeNode(grant.granted_at)),
      grant.expires_at !== null && grant.expires_at !== undefined
        ? add(el("p", "grant-expiry small muted"), el("span", "", "Expires "), timeNode(grant.expires_at))
        : el("p", "grant-expiry small muted", "Never expires"),
      grant.revoked_by ? add(el("p", "grant-revocation small muted"), el("span", "", "Revoked by "),
        refLink(grant.revoked_by)) : null);
    if (canManage && !grant.revoked_by) {
      const revokeForm = el("form", "grant-revoke-form");
      const revoke = focusKey(button(`Revoke ${grant.entry}`, () => {
        void submitMutation(revokeForm, { op: "revoke", entry: grant.entry }, (_, current) => {
          if (current) { notice("Grant revoked.", "success"); scheduleRefresh(); }
        });
      }, "compact"), `grant-revoke:${grant.entry}`);
      revokeForm.append(revoke);
      item.append(retainForm(revokeForm, `grant-revoke:${grant.entry}`));
    }
    return item;
  }

  function createForm(plan) {
    const form = el("form", "grant-form");
    const scope = field("Grant scope", "grantee_scope", "session", {
      choices: [["session", "Exact session"], ["harness", "Harness-wide"]],
    });
    const recipient = field("Recipient", "grantee", "", {
      required: true, maxLength: 768, placeholder: "user@host/harness/session",
      hint: "Exact session accepts user@host/harness/session or harness/session. Select Harness-wide to target a bare harness label.",
    });
    const expiry = field("Expires after", "duration", "", { choices: GRANT_DURATIONS });
    const note = field("Note", "note", "", { maxLength: 512, hint: "optional reason for this grant" });
    const roleFieldset = el("fieldset", "grant-role-picker");
    const legend = el("legend", "", "Roles");
    roleFieldset.append(legend);
    for (const role of AUTHORITY_ROLES) {
      const input = el("input"); input.type = "checkbox"; input.name = "roles"; input.value = role;
      focusKey(input, `grant-role:${plan.id}:${role}`);
      const label = el("label", "grant-role-choice");
      label.append(input, el("span", "", role)); roleFieldset.append(label);
    }
    const submit = focusKey(el("button", "primary", "Grant access"), `grant-submit:${plan.id}`);
    submit.type = "submit";
    add(form, scope.wrapper, recipient.wrapper, roleFieldset, expiry.wrapper, note.wrapper, submit);
    form.addEventListener("submit", event => {
      event.preventDefault();
      const roles = [...roleFieldset.querySelectorAll("input")]
        .filter(input => input.name === "roles")
        .filter(input => input.checked).map(input => input.value);
      if (!roles.length) { formStatus(form, "Choose at least one role."); return; }
      const grantee = recipient.input.value;
      if (!validGrantee(grantee, scope.input.value)) {
        formStatus(form, scope.input.value === "harness"
          ? "Enter one valid bare harness label for harness-wide access."
          : "Enter an exact user@host/harness/session identity or harness/session shorthand.");
        return;
      }
      const parsedSeconds = Number(expiry.input.value);
      const seconds = Number.isSafeInteger(parsedSeconds) && parsedSeconds > 0 ? parsedSeconds : null;
      void submitMutation(form, {
        op: "grant", plan: plan.id, roles, grantee,
        expires_in: seconds, note: note.input.value || null,
      }, (_, current) => {
        if (current) { notice("Grant recorded.", "success"); scheduleRefresh(); }
      });
    });
    return retainForm(form, `grant:${plan.id}`);
  }

  function grantPanel(view) {
    const grants = Array.isArray(view.grants) ? view.grants : [];
    const now = (Date.now() + (Number(state.clockOffsetMs) || 0)) / 1000;
    const canManage = view.can_manage_grants === true;
    const rows = grants.slice().sort((left, right) => Number(right.granted_at || 0) - Number(left.granted_at || 0))
      .map(grant => grantRow(grant, canManage, now));
    const content = el("div", "grant-content");
    content.append(rows.length ? add(el("div", "grant-list"), rows) : el("p", "empty", "No plan grants."));
    if (canManage) content.append(createForm(view.plan));
    else content.append(el("p", "small muted grant-readonly", "This session cannot manage plan grants."));
    return panel("Plan grants", content, "grant-panel");
  }

  return { grantPanel };
}
