// A walk through every tab against a real stack, one page throughout, as a
// person would click through it. See the README for the stack it needs.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { type Page, type Request, expect, test } from "@playwright/test";

const RUN = process.env["PISA_E2E_RUN"] ?? "0";
const API = process.env["PISA_URL"] ?? "http://localhost:9100";
const OUT = fileURLToPath(new URL("./out/", import.meta.url));
const LABELS = `e2e=${RUN}`;
const ID = /~[0-9a-f]{32}$/;

test.describe.configure({ mode: "serial" });

let page: Page;
test.beforeAll(async ({ browser }) => {
  page = await browser.newPage();
  await page.goto("/");
});
test.afterAll(async () => {
  await page.close();
});

const tab = (name: string) => page.getByRole("tab", { name, exact: true }).click();
const region = (name: string | RegExp) => page.getByRole("region", { name });
const shot = (name: string) => page.screenshot({ path: `${OUT}${name}.png`, fullPage: true });

/** Replaces an editor's code. */
async function fill(editor: string, code: string) {
  await page.getByRole("textbox", { name: editor }).click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.insertText(code);
}

/** `docker compose` on the stack the walk runs against. */
function compose(...args: string[]) {
  execFileSync("docker", ["compose", ...args], { stdio: "inherit" });
}

/** A snapshot id's short form, as a link to it shows it. */
const short = (id: string) => id.split("~")[1]?.slice(0, 8) ?? "";

interface Created {
  snapshot?: { id: string; module: string; header: string };
  heads?: string[];
}

/** Waits for the answer to the create that `act` sends, and for the page to
 *  show what it kept. */
async function answer(act: () => Promise<void>): Promise<Created> {
  const response = page.waitForResponse((r) => r.request().method() === "POST" && new URL(r.url()).pathname === "/api/snapshots");
  await act();
  const json = (await (await response).json()) as Created;
  if (json.snapshot !== undefined) await expect(region("Keep code as a snapshot").locator(`[title="${json.snapshot.id}"]`)).toBeVisible();
  return json;
}

/** Creates a snapshot from the Snapshots tab. */
async function create(code: string, step: string, fields: Record<string, string> = {}): Promise<Created> {
  const panel = region("Keep code as a snapshot");
  await fill("Snapshot code", code);
  await panel.getByLabel("Labels (a=1, b=2)").fill(`${LABELS}, step=${step}`);
  await panel.getByLabel("Kernel re-check").selectOption("yes");
  await panel.getByLabel("Replaces (a snapshot id)").fill(fields["replaces"] ?? "");
  if (fields["replaces"] === undefined) await panel.getByLabel("Holes (definitions)").fill(fields["holes"] ?? "");
  else await panel.getByLabel("May change (definitions)").fill(fields["allow"] ?? "");
  return answer(() => panel.getByRole("button", { name: "Create" }).click());
}

/** The id of the snapshot a create kept. */
function kept(json: Created): string {
  expect(json.snapshot).toBeDefined();
  return json.snapshot?.id ?? "";
}

/** The snapshots the walk makes, as it makes them. */
const made = { base: "", dep: "", spec: "", head: "", headModule: "", headHeader: "", base2: "" };

const BASE = `def e2eDouble (n : Nat) : Nat := 2 * n

theorem e2e_double (n : Nat) : e2eDouble n = n + n := by
  unfold e2eDouble
  omega`;

const SPEC = (limit: number, step: number, proof: string) => `def e2eLimit : Nat := ${limit}
def e2eStep : Nat := ${step}

theorem e2e_main (n : Nat) (h : n < e2eLimit) : n < e2eLimit + e2eStep := by
  ${proof}`;
const PROOF = "unfold e2eLimit e2eStep at *\n  omega";

test("the version box sends a version only once it settles", async () => {
  const sent: unknown[] = [];
  const seen = (r: Request) => {
    if (r.method() === "POST" && new URL(r.url()).pathname === "/api/check") sent.push(r.postDataJSON()?.version);
  };
  page.on("request", seen);
  const box = page.getByPlaceholder("the server's default");
  await box.pressSequentially("v4.3", { delay: 80 });
  await expect.poll(() => sent.includes("v4.3")).toBe(true);
  expect(sent.filter((v) => v !== undefined && v !== "v4.3")).toEqual([]);
  await box.fill("");
  page.off("request", seen);
});

test("check: statuses, sorry positions, pins with terms, declarations in full, a batch", async () => {
  await tab("Check");
  const result = region("Result");
  await fill("Code to check", "theorem e2e_s (n : Nat) : n + 0 = n := by\n  sorry");
  await expect(result.getByText("Sorries", { exact: true })).toBeVisible();
  await expect(result.getByText("2:2", { exact: true })).toBeVisible();
  await expect(result.getByText("sorry", { exact: true })).toBeVisible();

  await fill("Code to check", "theorem e2e_p (n : Nat) : n + 0 = n := by\n  exact Nat.add_zero n");
  await expect(result.getByText("✓ accepted")).toBeVisible();
  await expect(result.getByText("proved", { exact: true })).toBeVisible();
  // On the `n` that ends the proof.
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.press("ArrowLeft");
  await page.getByRole("button", { name: "Pin the cursor" }).click();
  await expect(page.getByRole("button", { name: "Unpin 2:21" })).toBeVisible();
  const pinned = region("At pinned positions");
  await expect(pinned.getByText("exact Nat.add_zero n")).toBeVisible();
  await page.getByRole("checkbox", { name: "Terms at the pins" }).check();
  await expect(pinned.getByText("Expected type")).toBeVisible();

  await page.getByRole("checkbox", { name: "Declarations in full" }).check();
  await expect(result.getByRole("columnheader", { name: "Axioms", exact: true })).toBeVisible();
  await result.getByRole("button", { name: "e2e_p" }).click();
  await expect(result.getByText("type hash")).toBeVisible();
  await shot("check");

  const batch = region("Check a batch");
  await fill("Batch code", "theorem e2e_b1 : 1 + 1 = 2 := rfl\n-- ---\ntheorem e2e_b2 : 2 + 2 = 5 := by decide");
  await batch.getByRole("button", { name: "Check 2" }).click();
  await expect(batch.getByText("accepted", { exact: true })).toBeVisible();
  await expect(batch.getByText("not accepted", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Clear", exact: true }).click();
});

test("jobs: a check runs as a job", async () => {
  await page.getByRole("button", { name: "Check as a job" }).click();
  const job = region("As a job");
  await expect(job.getByText("done", { exact: true })).toBeVisible();
  await expect(job.getByText("✓ accepted")).toBeVisible();
});

test("jobs: one no worker has taken is withdrawn", async () => {
  test.skip(process.env["COMPOSE_PROJECT_NAME"] === undefined, "needs the compose stack, to stop its workers");
  await page.getByRole("checkbox", { name: "Check as I type" }).uncheck();
  compose("stop", "worker");
  try {
    await expect(page.getByText("0 workers")).toBeVisible({ timeout: 120_000 });
    await fill("Code to check", `theorem e2e_queued_${RUN} : True := trivial`);
    await page.getByRole("button", { name: "Check as a job" }).click();
    const job = region("As a job");
    await expect(job.getByText("queued", { exact: true })).toBeVisible();
    await job.getByRole("button", { name: "Cancel" }).click();
    await expect(job.getByText("cancelled", { exact: true })).toBeVisible();
    await shot("job-cancelled");
  } finally {
    compose("start", "worker");
  }
  await expect(page.getByText("2 workers")).toBeVisible({ timeout: 120_000 });
  await page.getByRole("checkbox", { name: "Check as I type" }).check();
});

test("documents: open, sorries, goals, probes, use one, save, a conflict, close", async () => {
  await tab("Documents");
  const doc = region("Document");
  await fill("Document code", "theorem e2e_d (a b : Nat) : a + b = b + a := by\n  sorry\n\nexample : 2 + 2 = 4 := by\n  sorry");
  await doc.getByRole("button", { name: "Open", exact: true }).click();
  await expect(doc.getByText("revision 1")).toBeVisible();
  const sorries = region("Sorries");
  await expect(sorries.getByText("sorry 2")).toBeVisible();
  await expect(sorries.getByText("⊢ a + b = b + a")).toBeVisible();

  const probes = region("Probes");
  await probes.getByRole("textbox", { name: "Candidates, one a line" }).fill("omega\nrfl\n#eval 1\nsorry");
  await probes.getByRole("button", { name: "Try 4" }).click();
  const result = (candidate: string) => probes.getByRole("listitem", { name: candidate, exact: true });
  await expect(result("omega")).toContainText("closes it");
  await expect(result("rfl")).toContainText("fails");
  await expect(result("#eval 1")).toContainText("policy");
  await expect(result("sorry")).toContainText("leaves 0");
  await shot("documents-probes");
  await result("omega").getByRole("button", { name: "Use it" }).click();
  await expect(doc.getByText("edited since")).toBeVisible();
  await doc.getByRole("button", { name: "Save as revision 2" }).click();
  await expect(doc.getByText("revision 2")).toBeVisible();
  await expect(sorries.getByText("sorry 2")).toBeHidden();

  const goals = region("Goals");
  await goals.getByPlaceholder("more lines: 2, 5").fill("2");
  await expect(goals.getByText("⊢ a + b = b + a")).toBeVisible();

  // An edit behind the page's back moves the document on.
  const id = (await doc.getByText(ID).textContent()) ?? "";
  const behind = await page.request.patch(`${API}/documents/${id}`, { data: { code: "theorem e2e_x : True := trivial", revision: 2 } });
  expect(behind.ok()).toBe(true);
  await page.getByRole("textbox", { name: "Document code" }).click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.insertText("\n-- edited");
  await doc.getByRole("button", { name: "Save as revision 3" }).click();
  await expect(page.getByRole("alert").filter({ hasText: "another revision" }).first()).toBeVisible();
  await doc.getByRole("button", { name: "Open again" }).click();
  await expect(doc.getByText("revision 1")).toBeVisible();
  await doc.getByRole("button", { name: "Close" }).click();
  await expect(page.getByText("Open the code to ask about its goals")).toBeVisible();
});

test("prove: candidates at a sorry, the tree, a check in full, a gone session, no Beam slot", async () => {
  await tab("Prove");
  const statement = region("Statement, with a sorry for each goal");
  await statement.getByRole("button", { name: "Start" }).click();
  const states = region("States");
  await expect(states.getByRole("button", { name: /sorry 2/ })).toBeVisible();

  const candidates = region("Try candidates at sorry 1");
  await candidates.getByRole("button", { name: "Try 4" }).click();
  const result = (candidate: string) => candidates.getByRole("listitem", { name: candidate, exact: true });
  await expect(result("simp")).toContainText("closes it");
  await expect(result("decide")).toContainText("fails");
  await result("simp").getByRole("button", { name: "Add to the tree" }).click();
  await expect(states.getByRole("button", { name: /simp.*done/ })).toBeVisible();

  // A tactic finishes while its tab is hidden.
  await states.getByRole("button", { name: /sorry 2/ }).click();
  await page.getByPlaceholder("a tactic, e.g. simp").fill("simp");
  await page.getByPlaceholder("a tactic, e.g. simp").press("Enter");
  await tab("Check");
  await tab("Prove");
  await expect(states.getByRole("button", { name: /simp.*done/ })).toHaveCount(2);

  const proof = region("The proof");
  await proof.getByRole("checkbox", { name: "Declarations in full" }).check();
  await proof.getByRole("button", { name: "Check it" }).click();
  await expect(proof.getByText("✓ accepted")).toBeVisible();
  await expect(proof.getByRole("columnheader", { name: "Axioms", exact: true })).toBeVisible();
  await page.getByRole("textbox", { name: "Statement" }).click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.insertText(" ");
  await expect(statement.getByText("edited since the start")).toBeVisible();
  await shot("prove");

  // Closed behind the page's back: the next tactic says so.
  const session = (await statement.getByText(ID).textContent()) ?? "";
  expect((await page.request.delete(`${API}/documents/${session}`)).status()).toBe(204);
  await states.getByRole("button", { name: /sorry 1/ }).click();
  await page.getByPlaceholder("a tactic, e.g. simp").fill("omega");
  await page.getByPlaceholder("a tactic, e.g. simp").press("Enter");
  await expect(page.getByRole("alert").filter({ hasText: "The session was closed, or has expired" }).first()).toBeVisible();
  await statement.getByRole("button", { name: "Close" }).click();
  await expect(states).toBeHidden();

  await page.route(
    (url) => url.pathname === "/api/sessions",
    (route) => route.fulfill({ status: 503, json: { error: { kind: "unavailable", message: "no Lean Beam slot" } } }),
  );
  await statement.getByRole("button", { name: "Start" }).click();
  await expect(page.getByRole("alert").filter({ hasText: "PISA_BEAM_SLOTS" })).toBeVisible();
  await page.unrouteAll();
});

test("snapshots: a verified create, verify with a timeout, dependents, details per snapshot", async () => {
  await tab("Snapshots");
  const panel = region("Keep code as a snapshot");
  const base = await create(BASE, "base");
  made.base = kept(base);
  await expect(panel.getByText("verified", { exact: true })).toBeVisible();
  made.dep = kept(await create(`${base.snapshot?.header ?? ""}\n\ntheorem e2e_dep : e2eDouble 1 = 2 := by decide`, "dep"));

  const list = region("Snapshots");
  await list.getByPlaceholder("filter by labels: a=1, b=2").fill(LABELS);
  await list.locator(`[title="${made.base}"]`).click();
  const detail = page.getByRole("region").filter({ hasText: "What builds on it" });
  await expect(detail.getByRole("button", { name: short(made.dep) })).toBeVisible();
  await detail.getByLabel("Verify within (s)").fill("120");
  await detail.getByRole("button", { name: "Verify" }).click();
  await expect(detail.getByRole("button", { name: "Verify" })).toBeEnabled();
  await expect(detail.getByRole("alert")).toHaveCount(0);
  await detail.getByText("The stored check").click();
  await detail.getByRole("checkbox", { name: "Declarations in full" }).check();
  await expect(detail.getByText("type hash").or(detail.getByRole("columnheader", { name: "Axioms", exact: true }))).toBeVisible();
  await shot("snapshot");

  // What was typed for one snapshot isn't there for the next.
  await page.getByPlaceholder("add k=v").fill("typed=1");
  await list.locator(`[title="${made.dep}"]`).click();
  await expect(page.getByPlaceholder("add k=v")).toHaveValue("");
});

test("replacing: fill, a refused change, a conflict's heads, holes and allow, a fork, unverified", async () => {
  const panel = region("Keep code as a snapshot");
  made.spec = kept(await create(SPEC(10, 1, "sorry"), "spec", { holes: "e2eLimit" }));
  await expect(panel.getByText("holes e2eLimit")).toBeVisible();

  const r1 = kept(await create(SPEC(10, 1, PROOF), "r1", { replaces: made.spec }));
  await expect(panel.getByText("Filled")).toBeVisible();

  const refused = await create(SPEC(10, 1, PROOF).replace("n < e2eLimit + e2eStep", "n ≤ e2eLimit + e2eStep"), "refused", { replaces: r1 });
  expect(refused.snapshot).toBeUndefined();
  await expect(panel.getByText("Nothing was kept.")).toBeVisible();
  await expect(panel.getByText("has another type")).toBeVisible();

  // The spec is replaced already: replace its head instead.
  const conflict = await create(SPEC(20, 1, PROOF), "r2", { replaces: made.spec });
  expect(conflict.heads).toEqual([r1]);
  await panel.getByRole("button", { name: "Replace this head" }).click();
  await expect(panel.getByLabel("Replaces (a snapshot id)")).toHaveValue(r1);
  const r2 = kept(await answer(() => panel.getByRole("button", { name: "Create" }).click()));
  await expect(panel.getByText("Changed definitions")).toBeVisible();
  await expect(panel.getByText("Theorems that use a changed definition")).toBeVisible();

  const r3 = await create(SPEC(20, 2, PROOF), "r3", { replaces: r2, allow: "e2eStep" });
  made.head = kept(r3);
  made.headModule = r3.snapshot?.module ?? "";
  made.headHeader = r3.snapshot?.header ?? "";
  await expect(panel.getByText("Changed definitions")).toBeVisible();
  await shot("replacement");

  await panel.getByRole("checkbox", { name: "Fork the run" }).check();
  kept(await create(SPEC(10, 1, PROOF), "fork", { replaces: made.spec }));
  await expect(panel.getByText("Filled")).toBeVisible();
  await panel.getByRole("checkbox", { name: "Fork the run" }).uncheck();

  // A re-check that didn't finish, as the server would say it.
  await page.route(
    (url) => url.pathname === "/api/snapshots",
    async (route) => {
      if (route.request().method() !== "POST") return route.fallback();
      const response = await route.fetch();
      const json = await response.json();
      json.unverified = { module: json.snapshot.module, reason: "ran out of time" };
      json.snapshot.verified = false;
      await route.fulfill({ response, json });
    },
  );
  kept(await create("theorem e2e_unverified : True := trivial", "unverified"));
  await page.unrouteAll();
  await expect(panel.getByText("the kernel re-check didn't finish")).toBeVisible();
  await panel.getByRole("status").getByRole("button", { name: "Verify" }).click();
  await expect(panel.getByText("the kernel re-check didn't finish")).toBeHidden();
  await expect(panel.getByText("verified", { exact: true })).toBeVisible();
});

test("runs: audits by labels and against the spec, a fork dropped, the review", async () => {
  await tab("Runs");
  const audit = region("Audit");
  await audit.getByLabel("Labels (a=1, b=2)").fill(LABELS);
  await audit.getByRole("button", { name: "Audit", exact: true }).click();
  await expect(audit.getByText(/^\d+ proved$/)).toBeVisible();
  await expect(audit.getByText(/^1 sorry$/)).toBeVisible();

  await audit.getByRole("combobox", { name: "Audit" }).selectOption("against");
  await audit.getByLabel("Spec snapshots (ids)").fill(made.spec);
  await audit.getByLabel("Also allow (definitions)").fill("e2eStep");
  await audit.getByRole("checkbox", { name: /With the review/ }).check();
  await audit.getByRole("button", { name: "Audit", exact: true }).click();
  await expect(audit.getByText("Not clean yet")).toBeVisible();
  const drops = audit.getByRole("button", { name: "Drop this branch" });
  await expect(drops).toHaveCount(2);
  await drops.nth(1).click();
  await audit.getByRole("button", { name: "Delete it and what replaces it?" }).click();
  await expect(audit.getByText("Clean: the run is done.")).toBeVisible();
  await expect(audit.getByText("hole", { exact: true }).first()).toBeVisible();
  await expect(audit.locator("pre.bg-amber-50", { hasText: "20" })).toBeVisible();
  await shot("audit");
});

test("runs: the export, and the run's acceptance", async () => {
  test.setTimeout(900_000);
  const accept = region("Export and accept a run");
  await accept.getByLabel("Spec snapshots (ids)").fill(made.spec);
  await accept.getByLabel("Also allow (definitions)").fill("e2eStep");
  const download = page.waitForEvent("download");
  await accept.getByRole("button", { name: "Export" }).click();
  const saved = `${OUT}pisa-run.tar.gz`;
  await (await download).saveAs(saved);
  expect(execFileSync("tar", ["-tzf", saved], { encoding: "utf8" })).toContain("comparator/1.json");

  // Lean's other kernels run unless asked not to.
  const kernels = accept.getByRole("checkbox", { name: /external kernels/ });
  await expect(kernels).toBeChecked();
  await kernels.uncheck();
  const sent = page.waitForRequest((r) => r.method() === "POST" && new URL(r.url()).pathname === "/api/snapshots/accept");
  await accept.getByRole("button", { name: "Accept", exact: true }).click();
  expect((await sent).postDataJSON()).toMatchObject({ paranoid: false });
  await expect(accept.getByText("Accepted: the audit is clean, and comparator accepted every pair.")).toBeVisible({ timeout: 840_000 });
  await shot("accept");
});

test("runs: a rebuild onto a replacement, planned and run; a delete with what replaces it", async () => {
  await tab("Snapshots");
  const panel = region("Keep code as a snapshot");
  made.base2 = kept(await create(`${BASE}\n\ntheorem e2e_double_zero : e2eDouble 0 = 0 := rfl`, "base2", { replaces: made.base }));
  await panel.locator(`[title="${made.base2}"]`).click();
  const detail = page.getByRole("region").filter({ hasText: "What builds on it" });
  await detail.getByRole("button", { name: "Rebuild onto it" }).click();

  const rebuild = region("Rebuild");
  await expect(rebuild.getByLabel("From: the snapshot replaced")).toHaveValue(made.base);
  await expect(rebuild.getByLabel("To: its replacement")).toHaveValue(made.base2);
  await rebuild.getByLabel("Labels of the projects to follow (required)").fill(`${LABELS}, step=dep`);
  await rebuild.getByRole("button", { name: "Plan" }).click();
  await expect(rebuild.getByText("Level 1")).toBeVisible();
  await expect(rebuild.getByText("linked")).toBeVisible();
  await rebuild.getByRole("button", { name: "Run", exact: true }).click();
  await expect(rebuild.getByText("Every item was created")).toBeVisible({ timeout: 240_000 });
  await shot("rebuild");

  // A link to another run starts the panels it fills afresh: no plan, run
  // or verdict of the run before stays next to the new ids.
  await tab("Snapshots");
  const list = region("Snapshots");
  await list.locator(`[title="${made.base2}"]`).click();
  await detail.getByRole("button", { name: "Audit against it" }).click();
  const audit = region("Audit");
  await expect(audit.getByLabel("Spec snapshots (ids)")).toHaveValue(made.base2);
  await expect(audit.getByLabel("Also allow (definitions)")).toHaveValue("");
  await expect(audit.getByRole("checkbox", { name: /With the review/ })).not.toBeChecked();
  const accept = region("Export and accept a run");
  await expect(accept.getByLabel("Spec snapshots (ids)")).toHaveValue(made.base2);
  await expect(accept.getByText("Accepted: the audit is clean, and comparator accepted every pair.")).toBeHidden();
  await tab("Snapshots");
  await detail.getByRole("button", { name: "Rebuild from it" }).click();
  await expect(rebuild.getByLabel("From: the snapshot replaced")).toHaveValue(made.base2);
  await expect(rebuild.getByLabel("To: its replacement")).toHaveValue("");
  await expect(rebuild.getByText("Level 1")).toBeHidden();
  await expect(rebuild.getByText("Every item was created")).toBeHidden();

  await tab("Snapshots");
  await list.locator(`[title="${made.base}"]`).click();
  await detail.getByRole("checkbox", { name: "Delete what replaces it too" }).check();
  await detail.getByRole("button", { name: "Delete" }).click();
  await detail.getByRole("button", { name: "Delete it and what replaces it?" }).click();
  const gone = page.getByRole("status").filter({ hasText: "Deleted" });
  await expect(gone).toContainText(short(made.base));
  await expect(gone).toContainText(short(made.base2));
});

test("search: hits link to their snapshot and to Check", async () => {
  await page.route(
    (url) => url.pathname === "/api/health",
    async (route) => {
      const response = await route.fetch();
      const json = await response.json();
      json.search = { ready: true, version: "e2e", qdrant: true };
      await route.fulfill({ response, json });
    },
  );
  let asked: { queries: Record<string, unknown>[] } | undefined;
  await page.route(
    (url) => url.pathname === "/api/search",
    async (route) => {
      asked = route.request().postDataJSON();
      const hit = {
        name: "e2e_main",
        kind: "theorem",
        library: "PisaSnap",
        module: made.headModule,
        signature: "theorem e2e_main (n : Nat) (h : n < e2eLimit) : n < e2eLimit + e2eStep",
        flags: { imports: made.headHeader },
        score: 1,
        text: "e2e_main: a bound",
        snapshot_ids: [made.head],
        status: "proved",
        verified: true,
      };
      await route.fulfill({ json: { version: "e2e", results: [{ hits: [hit] }] } });
    },
  );
  await expect(page.getByRole("tab", { name: "Search" })).toBeVisible({ timeout: 15_000 });
  await tab("Search");
  await page.getByRole("textbox", { name: "anything" }).fill("a bound");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await expect(page.getByText("1 hit")).toBeVisible();
  expect(asked?.queries[0]).not.toHaveProperty("include_sorried");
  await shot("search");

  await page.getByRole("button", { name: short(made.head) }).click();
  await expect(page.getByRole("tab", { name: "Snapshots" })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("region").filter({ hasText: "What builds on it" }).getByText(made.head)).toBeVisible();

  await tab("Search");
  await page.getByRole("button", { name: "Try in Check" }).click();
  await expect(page.getByRole("tab", { name: "Check" })).toHaveAttribute("aria-selected", "true");
  await expect(region("Result").getByText("✓ accepted")).toBeVisible();
  await page.unrouteAll();
});
