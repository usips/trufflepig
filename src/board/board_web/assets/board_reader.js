export function createBoardReader(context) {
  const {
    state, dom, views, board, jsonFetch, apiVersion, readOp, snapshotSeq, minimumSeq, cursorFromRoute,
    parseBoardJson, queryFilters,
  } = context;
  const { el, add, title, panel, planId, actorName, stamp } = dom;
  const {
    renderOverview, renderAttention, renderClaims, feedbackPage, searchPage, editorPage, entryPage,
    entriesPage, diffPage, sanitizedMarkup, renderPlan,
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
  async function fetchRoute(route, signal, forceSnapshot) {
    const replies = [];
    const readReply = async op => {
      const reply = await board(op, signal);
      if (snapshotSeq(reply) === null) throw new Error("A board read is missing its snapshot sequence.");
      replies.push(reply); return reply;
    };
    const read = async op => (await readReply(op)).data;
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
        { scope: "all", after: page.after || null, through: page.through || null, limit: 50 }));
      const watermark = snapshotSeq(reply);
      const after = String(BigInt(watermark) > 20n ? BigInt(watermark) - 20n : 0n);
      const feed = await read(readOp("feed", { scope: "all", plan: null, after, through: watermark, limit: 20 }));
      return { ...reply.data, events: feed.events };
    };
    const readAttention = (page = {}) => read(readOp("attention",
      { scope: "all", after: cursorFromRoute(page, true), through: page.through || null, limit: 50 }));
    const readClaims = (plan, after, through, ownStale = false) => read(readOp("claims", {
      plan, own_stale: ownStale, scope: "all",
      after: after ? parseBoardJson(after) : null, through: through || null, limit: 50,
    }));
    let page, overview = null, attention = null;
    if (route.view === "overview") {
      [overview, attention] = await Promise.all([readOverview(route), readAttention()]);
      page = renderOverview(overview, attention, route);
    } else if (route.view === "attention") {
      attention = await readAttention(route); page = renderAttention(attention, route);
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
          page = add(el("div"),
            title(`Plan revision · ${view.id}`,
              `${view.source} · ${actorName(view.actor)} · ${stamp(view.created_at)}`),
            panel("", sanitizedMarkup(await renderedReply("plan", route.ref))));
        } else {
          const [rendered, extra] = await Promise.all([
            renderedReply("plan", view.revision.id),
            route.tab === "history" ? read(readOp("history",
              { plan: view.plan.id, after: cursorFromRoute(route), through: route.through || null, limit: 50 })) :
              route.tab === "entries" ? entriesNewestPage(route, readReply, view.plan.id) :
                route.tab === "tasks" ? Promise.all([
                  read(readOp("tasks", {
                    plan: view.plan.id, after: route.taskAfter || null, through: route.taskThrough || null,
                    ceiling: route.taskCeiling ? parseBoardJson(route.taskCeiling) : null, limit: 50,
                  })),
                  readClaims(view.plan.id, route.claimAfter, route.claimThrough || view.through),
                ]).then(([tasks, claims]) => ({ tasks, claims })) : Promise.resolve(null),
          ]);
          page = renderPlan(view, rendered, extra, route);
        }
      }
    } else throw new Error("This board page does not exist.");
    if (route.view !== "overview" && (forceSnapshot || state.watermark === null)) {
      overview = await readOverview();
      if (!attention) attention = await readAttention();
    }
    const times = replies.map(reply => reply.data?.server_now).filter(time => Number.isFinite(time));
    return { page, overview, attention, serverNow: times.length ? Math.max(...times) : null,
      watermark: minimumSeq(replies), warnings: replies.flatMap(reply => reply.warnings) };
  }

  return { fetchRoute };
}
