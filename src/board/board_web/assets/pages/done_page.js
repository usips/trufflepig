export function createDonePage({ state, dom, taskCard }) {
  const { el, add, panel, empty, link, title, timeNode } = dom;

  function completionGroup(doneAt, today, tomorrow, week) {
    if (!Number.isFinite(doneAt)) return "Earlier";
    const completed = doneAt * 1000;
    if (completed >= today && completed < tomorrow) return "Today";
    return completed >= week && completed < today ? "This week" : "Earlier";
  }

  function doneTasks(data, route) {
    const content = el("div", "done-history");
    const now = new Date(Date.now() + state.clockOffsetMs);
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    const tomorrow = new Date(today); tomorrow.setDate(tomorrow.getDate() + 1);
    const week = new Date(today); week.setDate(week.getDate() - (week.getDay() + 6) % 7);
    const groups = new Map([ ["Today", []], ["This week", []], ["Earlier", []] ]);
    for (const task of data.tasks) {
      groups.get(completionGroup(task.done_at, today.getTime(), tomorrow.getTime(), week.getTime())).push(task);
    }
    for (const [label, tasks] of groups) {
      if (!tasks.length) continue;
      const cards = tasks.map(task => {
        const card = taskCard(task, []);
        if (Number.isFinite(task.done_at)) card.append(add(el("div", "small muted done-time"),
          el("span", "", "Done "), timeNode(task.done_at)));
        return card;
      });
      content.append(panel(label, add(el("div", "done-cards"), cards), "done-group"));
    }
    if (!data.tasks.length) content.append(empty("No completed tasks."));
    if (data.next_before) content.append(link("Load older", route.view, {
      ...route, before: JSON.stringify(data.next_before),
    }, "button"));
    if (route.before) content.append(link("Latest Done", route.view, { ...route, before: null }, "button"));
    return content;
  }

  function renderDone(data, route) {
    return add(el("div"), title("Done", "Completed tasks across the selected project."), doneTasks(data, route));
  }
  return { doneTasks, renderDone };
}
