# @aqora/pisa-client

A typed client for pisa's HTTP API (Lean checks, proof sessions, snapshots and
declaration search), with no dependencies. It runs wherever `fetch` does. For
React, see `@aqora/pisa-react`.

```ts
import { PisaClient, isLeanError, proofText } from "@aqora/pisa-client";

const pisa = new PisaClient({ url: "http://localhost:8000", version: "v4.35.0-da683984" });

const result = await pisa.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num");
result.ok; // the verdict; a check that couldn't complete has `error` set

const code = "theorem t (n : Nat) : n + 0 = n := by sorry";
const { session, states } = await pisa.startSession(code);
const step = await pisa.tactic(states[0]!.state, "simp"); // states stay valid: branch freely
const proof = proofText(code, [{ sorry: states[0]!, tactics: ["simp"] }]);
(await pisa.checkSession(session, proof)).ok;

const base = await pisa.createSnapshot("import Mathlib\ndef double (n : Nat) := 2 * n");
// Kept only if it keeps the statements of the entry it replaces.
const more = "import Mathlib\ndef double (n : Nat) := 2 * n\ntheorem double_two : double 2 = 4 := rfl";
await pisa.createSnapshot(more, { replaces: base.entry! });
await pisa.check(`${base.header}\nexample : double 2 = 4 := rfl`);

const { hits } = await pisa.search({ nl: "trace of a density matrix is one" });
```

- **Errors.** Every method throws a `PisaError` whose `kind` is one of the
  server's error kinds, or `network`, `aborted` or `bad_response`. The helpers
  `isLeanError`, `isLimitError`, `isCapacityError` and `isNotFound` group the
  kinds.
- **Cancelling.** Every method takes a `signal`. Aborting gives the job up only
  if no worker has claimed it yet.
- **Long requests.** Behind a proxy that cuts requests off, `submitCheck`,
  `submitSession`, `submitTactic`, `submitSessionCheck`, `submitSnapshot` and
  `submitVerify` ask for an async request and return its `Job` at once.
  `waitJob(job)` then returns what the call would have, or throws what it would
  have thrown; `pollJob(job, { wait })` looks once, and `cancelJob(job)`
  withdraws a job no worker has started. A job's answer is kept for an hour.
  ```ts
  const job = await pisa.submitCheck("import Mathlib\nexample : 2 + 2 = 4 := by norm_num");
  (await pisa.waitJob(job)).ok;
  ```
- **Options.** `version`, `timeout` and `envId` can be set per call, or for the
  whole client.
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
