import { actorAttribution, agentMark, harnessVendor } from "./marks/agent_marks.js";

export function createBoardTaskView(context) {
  const { dom, entryCard, workerTree, claimSignals } = context;
  const { el, add, badge, refLink, timeNode, omitted } = dom;

  function claimHistory(items, entries, view) {
    const details = el("details", "task-claim-history");
    add(details, el("summary", "", `Claim history (${items.length})`));
    const list = el("ol", "claim-history-list");
    for (const claim of items) {
      const row = el("li", "claim-history-row");
      const source = entries.find(entry => entry?.id === claim.entry);
      add(row, refLink(claim.entry), actorAttribution({ ...claim,
        origin: source?.origin, authority: source?.authority,
      }),
        claim.model || claim.effort
          ? el("span", "muted small", [claim.model, claim.effort].filter(Boolean).join(" · ")) : null,
        timeNode(claim.claimed_at),
        claim.end_reason ? badge(claim.end_reason) : null,
        claim.ended_at ? add(el("span", "muted small"), el("span", "", "Ended "), timeNode(claim.ended_at)) : null);
      if (claim.workers?.length || claim.workers_omitted || claim.progress) {
        const snapshot = el("details", "task-claim-snapshot");
        add(snapshot, el("summary", "", `Agent snapshot (${claim.workers?.length || 0} workers)`));
        snapshot.addEventListener("toggle", () => {
          if (!snapshot.open || snapshot.dataset.loaded) return;
          snapshot.dataset.loaded = "true";
          snapshot.append(workerTree(claim, {
            entries, now: view.server_now, ttlSecs: view.claim_ttl_secs,
          }));
        });
        row.append(snapshot);
      }
      list.append(row);
    }
    details.append(list);
    return details;
  }

  function commitCard(commit, task) {
    const row = el("li", "task-commit-row");
    add(row, refLink(commit.oid, commit.oid, "mono", { plan: String(task.id).split(".")[0], repo_key: commit.repo_key }),
      el("p", "task-commit-subject", commit.subject),
      add(el("p", "muted small"), el("span", "", `${commit.author} · `), timeNode(commit.committed_at)));
    if (commit.coauthors?.length) {
      const authors = el("div", "coauthor-list");
      for (const author of commit.coauthors) {
        const label = `${author.harness} · ${author.model} · ${author.email}`;
        add(authors, add(el("span", "coauthor"), agentMark(harnessVendor(author.harness), label),
          el("span", "", `${author.harness} (${author.model})`), el("span", "muted small", author.email)));
      }
      row.append(authors);
    }
    if (commit.worker_refs?.length) {
      row.append(add(el("div", "entry-refs task-commit-workers"),
        commit.worker_refs.map(worker => el("span", "mono", `${worker.task}/${worker.handle}`))));
    }
    if (commit.claim_links?.length) {
      const links = el("div", "entry-refs task-commit-claims");
      for (const claim of commit.claim_links) {
        add(links, refLink(claim.task), refLink(claim.claim), badge(claim.basis));
        if (claim.actor) links.append(actorAttribution({ actor: claim.actor,
          vendor: harnessVendor(claim.actor.harness) }));
      }
      row.append(links);
    }
    return row;
  }

  function taskViewDetails(view) {
    if (!view?.task) return null;
    const section = el("section", "task-view-detail");
    section.dataset.task = view.task.id;
    section.setAttribute("aria-label", `Task detail ${view.task.id}`);
    const current = view.current_claim;
    const summary = claimSignals({
      ...(current || {}), task: view.task.id,
      open_questions: view.open_questions,
      oldest_open_question: view.oldest_open_question,
      oldest_open_question_at: view.oldest_open_question_at,
      commit_count: 0, latest_commit: null, latest_commit_at: null,
    }, { now: view.server_now, ttlSecs: view.claim_ttl_secs, showZeroQuestions: true });
    add(section, el("h3", "", `Task detail · ${view.task.id}`), summary);

    if (current) {
      section.append(workerTree(current, {
        entries: view.entries, now: view.server_now, ttlSecs: view.claim_ttl_secs,
      }));
    } else {
      section.append(el("p", "small muted", "No current claim."));
    }
    if (view.claim_history?.length) {
      section.append(claimHistory(view.claim_history, view.entries || [], view));
    }
    section.append(omitted(view.claims_omitted, "claims") || el("span"));

    const entries = el("ol", "task-entry-list");
    for (const entry of view.entries || []) entries.append(entryCard(entry));
    if (entries.childElementCount || view.entries_omitted) {
      section.append(add(el("section", "task-view-entries"), el("h4", "", "Task entries"),
        entries.childElementCount ? entries : el("p", "empty", "No task entries in this snapshot."),
        omitted(view.entries_omitted, "task entries") || el("span")));
    }

    const commits = [...(view.commits || [])].sort((left, right) =>
      Number(right.committed_at || 0) - Number(left.committed_at || 0));
    const commitList = add(el("ol", "task-commit-list"), commits.map(commit => commitCard(commit, view.task)));
    const latest = commits[0];
    if (latest || view.latest_commit_at !== null && view.latest_commit_at !== undefined) {
      const metadata = add(el("p", "task-latest-commit small muted"),
        el("span", "", `${commits.length + Number(view.commits_omitted || 0)} linked commits`));
      if (latest) add(metadata, el("span", "", " · latest "), refLink(latest.oid, latest.oid, "mono",
        { plan: String(view.task.id).split(".")[0], repo_key: latest.repo_key }));
      if (view.latest_commit_at !== null && view.latest_commit_at !== undefined) {
        add(metadata, el("span", "", " · "), timeNode(view.latest_commit_at));
      }
      section.append(metadata);
    }
    if (commitList.childElementCount || view.commits_omitted) {
      section.append(add(el("section", "task-view-commits"), el("h4", "", "Task-linked commits"),
        commitList.childElementCount ? commitList : el("p", "empty", "No task-linked commits in this snapshot."),
        omitted(view.commits_omitted, "task commits") || el("span")));
    }
    return section;
  }

  return { taskViewDetails };
}
