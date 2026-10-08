import { boardReadScope, boardProjectSelector } from "./board_routing.js";
import { actorMark, harnessVendor } from "./marks/agent_marks.js";

export function createBoardReader(context) {
  const {
    state, dom, views, board, jsonFetch, apiVersion, readOp, snapshotSeq, minimumSeq, cursorFromRoute,
    parseBoardJson, queryFilters,
  } = context;
  const { el, add, title, panel, planId, actorName, timeNode } = dom;
  const {
    renderOverview, renderAttention, renderClaims, feedbackPage, searchPage, editorPage, entryPage,
    entriesPage, diffPage, sanitizedMarkup, renderPlan, renderDone, projectChips,
  } = views;
  // Newest-first paging over the descending Entries window: one read
  // per page, following the server's next-before cursor.
  async function entriesNewestPage(route, readReply, plan = null) {
    const data = (await readReply(readOp("entries", {
      ...queryFilters(route), plan: plan || route.plan || null,
    }))).data;
    if (!Array.isArray(data?.entries)) throw new Error("Board reply is missing its entries collection.");
    return data;
  }
  function doneClock(data) {
    if (Number.isFinite(data?.server_now)) state.clockOffsetMs = data.server_now * 1000 - Date.now();
  }
  function taskWindow(route, view) {
    const bounds = { after: route.taskAfter || null, through: route.taskThrough || null,
      ceiling: route.taskCeiling ? parseBoardJson(route.taskCeiling) : null };
    if (!route.task) return bounds;
    const target = /^(P[1-9]\d*)\.([1-9]\d*)$/.exec(route.task);
    if (!target || target[1] !== view.plan.id) throw new Error("Choose a task from the current plan.");
    const ordinal = BigInt(target[2]);
    if (route.taskAfter || ordinal <= 50n) return bounds;
    const ceiling = view.task_ceiling;
    if (ceiling?.plan !== view.plan.id || ordinal > BigInt(ceiling.ordinal)) {
      throw new Error("This task is outside the captured plan task window.");
    }
    return { after: String(ordinal - 1n), through: view.through, ceiling };
  }
  async function fetchRoute(route, signal, forceSnapshot) {
    const replies = [];
    let projects = state.projects || [], projectRecords = null;
    const readReply = async op => {
      signal?.throwIfAborted();
      const project = Object.hasOwn(op, "scope") ? boardProjectSelector(route.project) : undefined;
      let reply;
      try { reply = await board(op, signal, project); }
      catch (error) { if (projectRecords) error.projects = projectRecords; throw error; }
      signal?.throwIfAborted();
      if (snapshotSeq(reply) === null) throw new Error("A board read is missing its snapshot sequence.");
      replies.push(reply); return reply;
    };
    const read = async op => (await readReply(op)).data;
    if (state.projects === null || route.view === "overview" || forceSnapshot) {
      projectRecords = await read(readOp("projects"));
      if (!Array.isArray(projectRecords)) {
        throw new Error("Board reply is missing its projects collection.");
      }
      projects = projectRecords;
    }
    const renderedReply = async (kind, ref) => {
      const result = await jsonFetch(`/api/v1/render/${kind}/${encodeURIComponent(ref)}`, { signal });
      if (result.api !== apiVersion || (kind === "plan"
        ? typeof result.html !== "string" || !Array.isArray(result.headings)
        : !Array.isArray(result.hunks))) {
        throw new Error("Board returned incompatible rendered evidence.");
      }
      const snapshot = { seq: result.snapshot_seq, warnings: [] };
      if (snapshotSeq(snapshot) === null) throw new Error("Rendered evidence is missing its snapshot sequence.");
      replies.push(snapshot); return result;
    };
    const readOverview = async (page = {}) => {
      const reply = await readReply(readOp("overview",
        { scope: boardReadScope(route.project), after: page.after || null,
          through: page.through || null, limit: 50 }));
      const watermark = snapshotSeq(reply);
      const after = String(BigInt(watermark) > 20n ? BigInt(watermark) - 20n : 0n);
      const feed = await read(readOp("feed", {
        scope: boardReadScope(route.project), plan: null, after, through: watermark, limit: 20,
      }));
      return { ...reply.data, events: feed.events };
    };
    const readAttention = (page = {}) => read(readOp("attention",
      { scope: boardReadScope(route.project), after: cursorFromRoute(page, true),
        through: page.through || null, limit: 50 }));
    const readClaims = (plan, after, through, ownStale = false) => read(readOp("claims", {
      plan, own_stale: ownStale, scope: boardReadScope(route.project),
      after: after ? parseBoardJson(after) : null, through: through || null, limit: 50,
    }));
    let page, overview = null, attention = null;
    if (route.view === "overview") {
      [overview, attention] = await Promise.all([readOverview(route), readAttention()]);
      page = renderOverview(overview, attention, route, projects);
    } else if (route.view === "attention") {
      attention = await readAttention(route); page = renderAttention(attention, route);
    } else if (route.view === "done") {
      const completed = await read(readOp("done_tasks", {
        scope: boardReadScope(route.project), before: route.before ? parseBoardJson(route.before) : null,
        limit: 50,
      }));
      doneClock(completed); page = renderDone(completed, route);
    } else if (route.view === "claims") {
      page = renderClaims(
        await readClaims(route.plan || null, route.after, route.through, route.own_stale === "1"),
        route);
    } else if (route.view === "feedback") {
      const records = await read(readOp("feedback_list", {
        open_only: route.state !== "all",
        after: cursorFromRoute(route, true), through: route.through || null, limit: 20,
      }));
      const entryViews = new Map();
      for (let offset = 0; offset < records.feedback.length; offset += 4) {
        await Promise.all(records.feedback.slice(offset, offset + 4).map(async record => {
          entryViews.set(record.entry.id, await read(readOp("show", { target: record.entry.id })));
        }));
      }
      page = feedbackPage(records, route, entryViews);
    } else if (route.view === "search") {
      page = searchPage(
        route.q ? await read(readOp("search", { query: route.q, plan: route.plan || null, limit: 50 })) : null,
        route);
    } else if (route.view === "new") {
      page = editorPage("new", null, await read(readOp("repositories", { plan: null })));
    } else if (route.view === "entry") {
      const view = await read(readOp("show", { target: route.ref }));
      const [diff, repliesPage, backrefsPage] = await Promise.all([
        view.proposal ? renderedReply("proposal", route.ref) : Promise.resolve(null),
        route.repliesAfter ? read(readOp("entries", {
          ...queryFilters({}), kind: "answer", references: route.ref,
          after: parseBoardJson(route.repliesAfter), through: route.repliesThrough || null,
        })) : Promise.resolve(null),
        route.backrefsAfter ? read(readOp("entries", {
          ...queryFilters({}), references: route.ref,
          after: parseBoardJson(route.backrefsAfter), through: route.backrefsThrough || null,
        })) : Promise.resolve(null),
      ]);
      page = entryPage(view, diff, route, { replies: repliesPage, backrefs: backrefsPage });
    } else if (route.view === "entries") {
      page = entriesPage(await entriesNewestPage(route, readReply), route);
    } else if (["plan", "edit"].includes(route.view)) {
      if (!planId(route.ref)) throw new Error("Choose a plan using its P# reference.");
      if (route.view === "plan" && route.ref.includes("..")) {
        page = add(el("div"), title(`Revision diff · ${route.ref}`, "Complete stored revision differences."),
          diffPage(await renderedReply("diff", route.ref)));
      } else {
        const view = await read(readOp("show", { target: route.ref }));
        if (route.view === "edit") {
          const current = view.plan ? view : await read(readOp("show", { target: planId(route.ref) }));
          page = editorPage(route.view, { ...current, revision: view.plan ? view.revision : view });
        } else if (!view.plan) {
          const current = await read(readOp("show", { target: planId(route.ref) }));
          const actorLine = `${view.source} · ${actorName(view.actor)} · `;
          const heading = title(`Plan revision · ${view.id}`, actorLine);
          const detail = heading.querySelector("p"); detail.textContent = "";
          add(detail, actorMark({ ...view, vendor: harnessVendor(view.actor?.harness) }),
            el("span", "", actorLine), timeNode(view.created_at));
          page = add(el("div"),
            heading,
            projectChips(current.repo_keys, projects),
            panel("", sanitizedMarkup(await renderedReply("plan", route.ref))));
        } else {
          const [rendered, extra] = await Promise.all([
            renderedReply("plan", view.revision.id),
            route.tab === "history" ? read(readOp("history",
              { plan: view.plan.id, after: cursorFromRoute(route), through: route.through || null, limit: 50 })) :
              route.tab === "entries" ? entriesNewestPage(route, readReply, view.plan.id) :
                route.tab === "done" ? read(readOp("tasks", {
                  plan: view.plan.id, column: "done", order: "recent_first",
                  before: route.before ? parseBoardJson(route.before) : null,
                  after: null, ceiling: null, through: null, limit: 50,
                })) : route.tab === "tasks" ? Promise.all([
                  read(readOp("tasks", {
                    plan: view.plan.id, column: null, order: "ordinal", before: null,
                    ...taskWindow(route, view), limit: 50,
                  })),
                  readClaims(view.plan.id, route.claimAfter, route.claimThrough || view.through),
                ]).then(([tasks, claims]) => ({ tasks, claims })) : Promise.resolve(null),
          ]);
          if (route.tab === "done") doneClock(view);
          page = renderPlan(view, rendered, extra, route, projects);
        }
      }
    } else throw new Error("This board page does not exist.");
    if (route.view !== "overview"
      && (forceSnapshot || state.watermark === null || state.overview === null)) {
      overview = await readOverview();
      if (!attention) attention = await readAttention();
    }
    const times = replies.map(reply => reply.data?.server_now).filter(time => Number.isFinite(time));
    return { page, overview, attention, projects: projectRecords,
      serverNow: times.length ? Math.max(...times) : null,
      watermark: minimumSeq(replies), warnings: replies.flatMap(reply => reply.warnings) };
  }

  return { fetchRoute };
}
