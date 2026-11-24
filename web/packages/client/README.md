# @aqora/pisa-client

A typed client for pisa's HTTP API (Lean checks, proof sessions, snapshots and
declaration search), with no dependencies. It runs wherever `fetch` does. For
React, see `@aqora/pisa-react`.

```sh
npm install @aqora/pisa-client
```

The example needs no Mathlib, so it runs on a `PROJECT=lean-core` stack.

```ts
import { PisaClient, isLeanError, proofText } from "@aqora/pisa-client";

const pisa = new PisaClient({ url: "http://localhost:8000" });

const result = await pisa.check("theorem t : 2 + 2 = 4 := by decide");
result.ok; // the verdict; a check that couldn't complete has `error` set
// The tactic at line 1, column 28 (`decide`) and its goals; `detail: ["terms"]` adds terms and their expected types.
const { at } = await pisa.check("theorem t : 2 + 2 = 4 := by decide", { at: [{ line: 1, column: 28 }] });

const code = "theorem s (n : Nat) : n + 0 = n := by sorry";
const session = await pisa.startSession(code);
const sorry = session.states[0]!;
try {
  await pisa.applyTactic(sorry.id, "exact foo");
} catch (e) {
  if (!isLeanError(e)) throw e; // Lean rejected the tactic: a dead end, not a failure
}
const step = await pisa.applyTactic(sorry.id, "simp"); // states stay valid: branch freely
const proof = proofText(code, [{ sorry, tactics: ["simp"] }]);
(await pisa.checkSession(session.id, proof)).ok;
await pisa.closeSession(session.id); // and its states

// Code kept open on Lean Beam: goals at positions, and candidate tactics tried at a sorry.
const doc = await pisa.openDocument(code);
const { at: goals } = await pisa.documentGoals(doc.id, [{ line: 1 }]);
const { results } = await pisa.probeDocument(doc.id, { sorry: 0 }, ["simp", "omega", "rfl"]);
// An edit names the revision it replaces: another is a `conflict`.
const next = await pisa.editDocument(doc.id, doc.revision, code.replace("sorry", "simp"));
await pisa.closeDocument(doc.id);

const { snapshot: base, check } = await pisa.createSnapshot("def double (n : Nat) := 2 * n");
// `snapshot` is absent when nothing was kept: `refused` says what to do next, and `check` why.
await pisa.check(`${base!.header}\nexample : double 2 = 4 := rfl`);
// A merge patch: `null` removes a label, and the rest stay.
await pisa.updateSnapshot(base!.id, { labels: { project: "p", draft: null } });
// Kept only if it keeps the statements of the snapshot it replaces.
const more = "def double (n : Nat) := 2 * n\ntheorem double_two : double 2 = 4 := rfl";
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

// On a server that searches (with Qdrant and an index).
const { hits } = await pisa.search({ description: "trace of a density matrix is one" });
```

## Methods

Every method returns the response's JSON, or throws a `PisaError`. Its last
argument is optional, and takes a `signal` and, as the call needs them,
`version` and `timeout`. A `submit*` method sends its call as a job and
returns the `Job` at once (see Long requests below).

Server:

| Method | What it does |
| --- | --- |
| `health()` | The server's health. A draining server's 503 is returned, not thrown |
| `listEnvironments()` | Every environment a worker has registered, by version, with its live `pool` |

Checks:

| Method | What it does |
| --- | --- |
| `check(code, { detail?, at? })` | Checks code; `at` takes up to 64 positions to report the tactic and goals at |
| `submitCheck(code, options?)` | `check` as a job |
| `checkBatch(codes, options?)` | Checks up to 128 pieces of code, a result each, in order |

Sessions:

| Method | What it does |
| --- | --- |
| `startSession(code)` | Starts a session on code with `sorry`s, a state each |
| `submitStartSession(code)` | `startSession` as a job |
| `applyTactic(state, tactic)` | Applies a tactic to a state, which stays valid; a tactic Lean rejects throws `lean` |
| `submitApplyTactic(state, tactic)` | `applyTactic` as a job |
| `checkSession(session, code, { detail? })` | Checks the completed code from scratch against the session's statements |
| `submitCheckSession(session, code)` | `checkSession` as a job |
| `closeSession(session)` | Closes a session, and its states with it |

Documents, on Lean Beam:

| Method | What it does |
| --- | --- |
| `openDocument(code)` | Opens code, kept elaborated for goals and probes until closed or expired |
| `editDocument(id, revision, code)` | Replaces the code at `revision` with the next revision |
| `documentGoals(id, at, { revision?, mode? })` | The goals at lines or positions, `before` or `after` the tactic there |
| `probeDocument(id, at, candidates, { revision? })` | Runs candidate tactics at a `sorry` or a position; nothing stays |
| `closeDocument(id)` | Closes a document and deletes its code |

Snapshots:

| Method | What it does |
| --- | --- |
| `createSnapshot(code, options?)` | Checks code and keeps it as a module later code imports through its `header` |
| `submitCreateSnapshot(code, options?)` | `createSnapshot` as a job |
| `getSnapshot(id, { detail? })` | A snapshot with its module's code, check and exports |
| `listSnapshots({ labels?, limit?, after?, heads? })` | A page of snapshots, newest first; pass `next` back as `after` |
| `updateSnapshot(id, { labels })` | Changes labels as a merge patch |
| `deleteSnapshot(id, { withReplacers? })` | Deletes a snapshot, and the modules nothing else needs |
| `verifySnapshot(id, { timeout?, again? })` | Re-checks the module and its imports through the kernel |
| `submitVerifySnapshot(id, options?)` | `verifySnapshot` as a job |
| `listDependents(id, { labels? })` | The snapshots that import this one's module, directly or not, parents first |
| `rebuildSnapshots(from, to, labels, { dryRun?, allowChanges?, retire?, verify? })` | Plans, or runs, the creates that move `from`'s dependents onto `to` |
| `submitRebuildSnapshots(from, to, labels, options?)` | `rebuildSnapshots` as a job; a dry run is refused unsent |

Specs and runs:

| Method | What it does |
| --- | --- |
| `createSpec(code, { holes?, name? })` | Keeps a spec, whose `holes` its proofs may fill |
| `createProof(replacesId, code, { allow?, fork? })` | Keeps a proof that replaces a spec or a head; follows a single new head up to 3 times |
| `amendSpec(specId, reason, code, { holes? })` | Keeps a spec that amends another |
| `refute(specId, target, code)` | Keeps a refutation of one of a spec's targets |
| `auditSnapshots({ labels } \| { against, allow?, review?, reviewLimit? })` | Audits a project by its labels, or a run against its spec snapshots |
| `getRun(name)` | The run with that name: its root, heads, and what `import PisaRun.<name>` resolves to |
| `nameRun(name, snapshotId)` | Names the run a snapshot is in |
| `settleRun(against, { allow?, dryRun?, verify? })` | Rebuilds the heads whose targets hold only through links |
| `submitSettleRun(against, options?)` | `settleRun` as a job; a dry run is refused unsent |
| `resubmitSnapshots(from, to, { dryRun?, labels?, verify? })` | Carries a spec's run over to the spec that amends it |
| `submitResubmitSnapshots(from, to, options?)` | `resubmitSnapshots` as a job; a dry run is refused unsent |

Acceptance:

| Method | What it does |
| --- | --- |
| `exportRun(against, { allow?, profile?, pair?, challengeModule?, solutionModule? })` | The run as a Lake project, or with `profile: "palomar"` one pair as a Palomar package: tar.gz bytes |
| `acceptRun(against, { allow?, paranoid? }, { timeout? })` | Audits the run, runs `lake comparator` on its pairs, and stores the result |
| `submitAcceptRun(against, params?, options?)` | `acceptRun` as a job |
| `getAcceptance(id)` | A stored acceptance |
| `listAcceptances(spec)` | A spec's acceptances, newest first, at most 100 |

Jobs:

| Method | What it does |
| --- | --- |
| `pollJob(job, { wait? })` | Looks once, waiting up to `wait` seconds (at most 60): `{ done: true, answer }` or `{ done: false, job }` |
| `waitJob(job, { wait?, onPending? })` | Waits however long the job takes; returns or throws what its call would have |
| `cancelJob(job)` | Withdraws a job no worker has started |

Search, on a server that searches:

| Method | What it does |
| --- | --- |
| `search(query)` | Runs one query |
| `searchBatch(queries)` | Runs up to 64 queries, a result each, in order |
| `resolve(names, { snapshots? })` | Whether each name exists, with close names for those that don't |
| `getDeclaration(name, { module? })` | A declaration with its description, dependencies and users |
| `flagDescription(name, reason, { module? })` | Flags a declaration's description as wrong; returns its count of flags |

## Behaviour

- **Errors.** Every method throws a `PisaError` whose `kind` is one of the
  server's error kinds, or `network`, `aborted` or `bad_response`. A
  gateway's 502, 503 or 504 is `unavailable`. The helpers `isLeanError`,
  `isLimitError`, `isCapacityError`, `isNotFoundError` and `isConflictError`
  group the kinds; `isPisaError(e, ...kinds)` tests any. A check's or a
  create's own failures are in its result, not thrown.
- **Retries.** A request is sent again, after a random wait that grows each
  time and is at least the server's `Retry-After`: after a 429, which says
  nothing ran, and, for one safe to send twice (reads, checks, searches,
  tactics, goals, probes, verifies, creates and dry runs), after a 502, 503
  or 504 or a network failure. Up to 4 attempts within 5 minutes; `retry:
  false` sends each request once, and `retry: { attempts, baseDelayMs,
  maxDelayMs, deadlineMs }` changes the policy. A create without a `key` gets
  one of the client's, `client:` and 32 hex digits, so a retry can't keep it
  twice. A batch's items that no worker took are sent again alone.
- **Cancelling.** Every method takes a `signal`. Aborting gives the job up only
  if no worker has claimed it yet.
- **Long requests.** Behind a proxy that cuts requests off, the `submit*`
  methods ask for an async request and return its `Job` at once.
  `waitJob(job)` then returns what the call would have, or throws what it
  would have thrown, looking every 25 s; `pollJob(job, { wait })` looks once,
  and `cancelJob(job)` withdraws a job no worker has started. The server
  keeps a job's answer for an hour after it is done, a day for a snapshot's
  create and a run's acceptance. `waitJob(job, { wait: 5, onPending })` sees
  its progress, `queued` then `running`, every 5 seconds.
  ```ts
  const job = await pisa.submitCheck("theorem t : 2 + 2 = 4 := by decide");
  (await pisa.waitJob(job)).ok;
  ```
- **Options.** `version` and `timeout` can be set per call, or for the whole
  client; without them the server's default version and 60 s apply. Session,
  state and snapshot ids carry their version, so the calls that take one
  take no `version`; a create that `replaces`, `amends` or `refutes` a
  snapshot is built on that snapshot's version unless the call names one.
  `scope` sends every request in a scope, as the `Pisa-Scope` header.
- **Positions.** Lean's columns count code points. `posMapper(code)` converts
  them to string offsets and back, and `rangeOf(code, message)` gives the range
  a message covers. `proofText(code, proofs)` puts each proved `sorry`'s
  tactics in its place, for a session check.

The [client documentation](https://github.com/aqora-io/pisa/blob/main/docs/clients.md)
compares the clients, and the
[API documentation](https://github.com/aqora-io/pisa/blob/main/docs/api.md)
describes every endpoint.

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
