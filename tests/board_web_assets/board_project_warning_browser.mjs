// Run against an existing fixture Board Web listener; this script never starts a server.
// CLI: node <script> --playwright <absolute module> --chromium <absolute executable>
// --bootstrap-url-file <0600 URL file> --available-project <id> (repeat at least twice)
// --unavailable-project <id>. Set TMPDIR to a short, on-disk scratch directory.
// The exported assertion reuses an existing page and its served Projects response.
import assert from "node:assert/strict";
import { readFile, stat } from "node:fs/promises";
import { createRequire } from "node:module";
import { isAbsolute, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

async function observeProjectWarning(page, project) {
  await page.locator("#project").selectOption(project.id);
  await page.waitForFunction(({ id, hidden, title }) => {
    const warning = document.getElementById("project-warning");
    const query = new URLSearchParams(location.hash.split("?")[1] || "");
    return document.getElementById("project")?.value === id && query.get("project") === id
      && warning?.hidden === hidden && warning.title === title;
  }, { id: project.id, hidden: project.unavailable.length === 0,
    title: project.unavailable.map(member => member.name).join(", ") });
  const observation = await page.locator("#project-warning").evaluate(warning => {
    const rect = warning.getBoundingClientRect(), style = getComputedStyle(warning);
    return {
      selectedProjectId: document.getElementById("project").value,
      hidden: warning.hidden, hiddenAttribute: warning.getAttribute("hidden"),
      display: style.display, visibility: style.visibility,
      clientRects: warning.getClientRects().length, width: rect.width, height: rect.height,
      text: warning.textContent, title: warning.title,
    };
  });
  assert.equal(observation.selectedProjectId, project.id);
  return { projectId: project.id, name: project.name,
    unavailableCount: project.unavailable.length, ...observation };
}

export async function project_warning_is_hidden_for_available_projects(page, {
  projects, availableProjectIds, unavailableProjectId,
}) {
  assert.ok(Array.isArray(projects), "Projects must be the served response data");
  assert.ok(availableProjectIds.length >= 2, "Supply at least two available Project IDs");
  assert.equal(new Set(availableProjectIds).size, availableProjectIds.length);
  const available = availableProjectIds.map(id => {
    const project = projects.find(record => record.id === id);
    assert.ok(project, "Available Project must exist in the served Projects response");
    assert.deepEqual(project.unavailable, [], "Available Project must have no unavailable members");
    return project;
  });
  const unavailable = projects.find(record => record.id === unavailableProjectId);
  assert.ok(unavailable, "Positive control must exist in the served Projects response");
  assert.ok(unavailable.unavailable.length > 0, "Positive control must have unavailable members");
  const observations = { available: [], unavailable: null };
  observations.unavailable = await observeProjectWarning(page, unavailable);
  for (const project of available) {
    observations.available.push(await observeProjectWarning(page, project));
  }
  try {
    for (const observation of observations.available) {
      assert.equal(observation.hidden, true, observation.name + ": hidden property");
      assert.equal(observation.display, "none", observation.name + ": computed display");
      assert.equal(observation.clientRects, 0, observation.name + ": rendered warning rectangles");
      assert.equal(observation.width, 0, observation.name + ": warning width");
      assert.equal(observation.height, 0, observation.name + ": warning height");
    }
    const shown = observations.unavailable;
    assert.equal(shown.hidden, false, shown.name + ": warning must remain unhidden");
    assert.notEqual(shown.display, "none", shown.name + ": warning must remain displayed");
    assert.ok(shown.clientRects > 0 && shown.width > 0 && shown.height > 0);
    assert.equal(shown.text, "Unavailable members");
    assert.equal(shown.title, unavailable.unavailable.map(member => member.name).join(", "));
  } catch (error) {
    error.projectWarnings = observations;
    throw error;
  }
  return observations;
}

function browserOptions(arguments_) {
  const options = { availableProjectIds: [] };
  const single = {
    "--playwright": "playwright", "--chromium": "chromium",
    "--bootstrap-url-file": "bootstrapUrlFile", "--unavailable-project": "unavailableProjectId",
  };
  for (let index = 0; index < arguments_.length; index += 2) {
    const option = arguments_[index], value = arguments_[index + 1];
    assert.ok(value && !value.startsWith("--"), "Each browser option requires a value");
    if (option === "--available-project") options.availableProjectIds.push(value);
    else {
      assert.ok(Object.hasOwn(single, option), "Unknown browser option");
      assert.equal(options[single[option]], undefined, "Duplicate browser option");
      options[single[option]] = value;
    }
  }
  for (const key of ["playwright", "chromium", "bootstrapUrlFile"]) {
    assert.ok(options[key] && isAbsolute(options[key]),
      key + " requires an explicit absolute path");
  }
  assert.ok(options.unavailableProjectId, "Supply an unavailable Project ID");
  const scratch = process.env.TMPDIR;
  assert.ok(scratch && isAbsolute(scratch), "Set TMPDIR to an absolute on-disk scratch directory");
  assert.ok(resolve(scratch) !== "/tmp" && !resolve(scratch).startsWith("/tmp/"));
  return options;
}

async function runProjectWarningBrowser(options) {
  const privateFile = await stat(options.bootstrapUrlFile);
  assert.equal(privateFile.mode & 0o777, 0o600, "Bootstrap URL file must have mode 0600");
  const bootstrapUrl = (await readFile(options.bootstrapUrlFile, "utf8")).trim();
  const address = new URL(bootstrapUrl);
  assert.equal(address.protocol, "http:");
  assert.ok(["127.0.0.1", "[::1]", "localhost"].includes(address.hostname));
  assert.match(address.hash, /^#token=[a-f0-9]{64}$/);
  const { chromium } = createRequire(import.meta.url)(options.playwright);
  const browser = await chromium.launch({ headless: true, executablePath: options.chromium });
  try {
    const page = await browser.newPage();
    const [response] = await Promise.all([
      page.waitForResponse(reply => new URL(reply.url()).pathname === "/api/v1/board"
        && reply.request().method() === "POST"
        && reply.request().postDataJSON()?.op?.op === "projects"),
      page.goto(bootstrapUrl),
    ]);
    assert.equal(response.status(), 200, "Projects read must succeed against the served fixture");
    const reply = await response.json();
    assert.equal(reply.result.result, "projects");
    const observations = await project_warning_is_hidden_for_available_projects(page, {
      projects: reply.result.data, availableProjectIds: options.availableProjectIds,
      unavailableProjectId: options.unavailableProjectId,
    });
    console.log(JSON.stringify(observations));
  } finally {
    await browser.close();
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  await test("project_warning_is_hidden_for_available_projects", async () => {
    try {
      await runProjectWarningBrowser(browserOptions(process.argv.slice(2)));
    } catch (error) {
      if (error.projectWarnings) console.error(JSON.stringify(error.projectWarnings));
      const redact = text => text.replace(/([#?&]token=)[a-f0-9]{64}/gi, "$1[redacted]");
      const safeError = new Error(redact(error.message));
      safeError.name = error.name;
      safeError.stack = redact(error.stack || safeError.stack);
      throw safeError;
    }
  });
}
