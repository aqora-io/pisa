# @aqora/pisa-client

A typed client for pisa's HTTP API (Lean checks, proof sessions, snapshots and
declaration search), with no dependencies. It runs wherever `fetch` does. For
React, see `@aqora/pisa-react`.

```ts
import { PisaClient, isLeanError, proofText } from "@aqora/pisa-client";

const pisa = new PisaClient({ url: "http://localhost:8000", version: "v4.36.0-rc2-1e541805" });

const result = await pisa.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num");
result.ok; // the verdict; a check that couldn't complete has `error` set
// The tactic at line 2, column 30 (`norm_num`) and its goals; `detail: ["terms"]` adds terms and their expected types.
const { at } = await pisa.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num", { at: [{ line: 2, column: 30 }] });

const code = "theorem t (n : Nat) : n + 0 = n := by sorry";
const session = await pisa.startSession(code);
const sorry = session.states[0]!;
const step = await pisa.applyTactic(sorry.id, "simp"); // states stay valid: branch freely
const proof = proofText(code, [{ sorry, tactics: ["simp"] }]);
(await pisa.checkSession(session.id, proof)).ok;

// Code kept open on Lean Beam: goals at positions, and candidate tactics tried at a sorry.
const doc = await pisa.openDocument(code);
const { at: goals } = await pisa.documentGoals(doc.id, [{ line: 1 }]);
const { results } = await pisa.probeDocument(doc.id, { sorry: 0 }, ["simp", "omega", "rfl"]);
// An edit names the revision it replaces: another is a `conflict`.
const next = await pisa.editDocument(doc.id, doc.revision, code.replace("sorry", "simp"));
await pisa.closeDocument(doc.id); // a session's id is its document's: closing it ends the session

const { snapshot: base, check } = await pisa.createSnapshot("import Mathlib\ndef double (n : Nat) := 2 * n");
// `snapshot` is absent when nothing was kept, and `check` says why.
await pisa.check(`${base!.header}\nexample : double 2 = 4 := rfl`);
// A merge patch: `null` removes a label, and the rest stay.
await pisa.updateSnapshot(base!.id, { labels: { project: "p", draft: null } });
// Kept only if it keeps the statements of the snapshot it replaces.
const more = "import Mathlib\ndef double (n : Nat) := 2 * n\ntheorem double_two : double 2 = 4 := rfl";
const { snapshot: replacement } = await pisa.createSnapshot(more, { replaces: { snapshot_id: base!.id } });
// What builds on a snapshot, and the creates that move it onto a replacement.
const { snapshots } = await pisa.listDependents(base!.id, { labels: { project: "p" } });
// What the project rests on: declarations by status, assumptions and what uses them.
const { summary, assumptions } = await pisa.auditSnapshots({ labels: { project: "p" } });
// A run against its spec: its heads, forks, each target's status there, and whether it is `clean`.
const { clean, targets } = await pisa.auditSnapshots({ against: [base!.id] });
// With `review`, each head's declarations, and what it gave its spec's holes, to read against the spec.
const { review } = await pisa.auditSnapshots({ against: [base!.id], review: true });
// The run as a Lake project for `lake comparator` (the bytes of `pisa-run.tar.gz`), and its acceptance:
// the audit and comparator on each pair, kept under `id` for `getAcceptance`.
const tarball = await pisa.exportRun([base!.id]);
const { accepted, pairs } = await pisa.waitJob(await pisa.submitAcceptRun([base!.id]));
// A spec's acceptances, newest first; one whose run has moved on says `stale`.
const { acceptances } = await pisa.listAcceptances(base!.id);
// Create each item a level at a time, with its `replaces`.
const { plan } = await pisa.rebuildSnapshots(base!.id, replacement!.id, { project: "p" }, { dryRun: true });
// Or let the server run it, retiring the originals the copies replace (not `base`); as a job, since it can take minutes.
const done = await pisa.waitJob(
  await pisa.submitRebuildSnapshots(base!.id, replacement!.id, { project: "p" }, { retire: true }),
);

const { hits } = await pisa.search({ description: "trace of a density matrix is one" });
```

- **Errors.** Every method throws a `PisaError` whose `kind` is one of the
  server's error kinds, or `network`, `aborted` or `bad_response`. A
  gateway's 502, 503 or 504 is `unavailable`. The helpers `isLeanError`,
  `isLimitError`, `isCapacityError` and `isNotFoundError` group the kinds.
- **Retries.** A request is sent again, after a random wait that grows each
  time and is at least the server's `Retry-After`: after a 429, which says
  nothing ran, and, for one safe to send twice (reads, checks, tactics,
  goals, probes, verifies, creates and dry runs), after a 502, 503 or 504 or a
  network failure. Up to 4 attempts within 5 minutes; `retry: false` sends
  each request once, and `retry: { attempts, baseDelayMs, maxDelayMs,
  deadlineMs }` changes the policy. A create without a `key` gets one of the
  client's, `client:` and 32 hex digits, so a retry can't keep it twice. A
  batch's items that no worker took are sent again alone.
- **Cancelling.** Every method takes a `signal`. Aborting gives the job up only
  if no worker has claimed it yet.
- **Long requests.** Behind a proxy that cuts requests off, `submitCheck`,
  `submitStartSession`, `submitApplyTactic`, `submitCheckSession`,
  `submitCreateSnapshot`, `submitVerifySnapshot`, `submitRebuildSnapshots`,
  `submitSettleRun`, `submitResubmitSnapshots` and `submitAcceptRun` ask for
  an async request and return its `Job` at once. `waitJob(job)` then returns
  what the call would have, or throws what it would have thrown, looking every
  25 s; `pollJob(job, { wait })` looks once, and `cancelJob(job)` withdraws a
  job no worker has started. A job's answer is kept for an hour.
  `waitJob(job, { wait: 5, onPending })` sees its progress, `queued` then
  `running`, every 5 seconds.
  ```ts
  const job = await pisa.submitCheck("import Mathlib\nexample : 2 + 2 = 4 := by norm_num");
  (await pisa.waitJob(job)).ok;
  ```
- **Options.** `version` and `timeout` can be set per call, or for the whole
  client. Session, state and snapshot ids carry their version, so the calls
  that take one take no `version`; a create that `replaces` a snapshot is
  built on that snapshot's version unless the call names one.
- **Positions.** Lean's columns count code points. `posMapper(code)` converts
  them to string offsets and back, and `rangeOf(code, message)` gives the range
  a message covers.

## Tests

- `pnpm test` runs the unit tests.
- `pnpm test:integration` runs against a real server and a fake-repl worker,
  after `cargo build -p pisa-server -p pisa-worker -p pisa-tests -p pisa-search`;
  set `PISA_TEST_QDRANT_URL` to include search.
- The types in `src/openapi.ts` are generated from the server's OpenAPI
  document, `openapi.json` at the repository's root, and `src/types.ts` names
  them. After changing the API, run
  `PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server --test openapi`, then
  `pnpm -C web generate`; `pnpm test` fails until the types are current.
