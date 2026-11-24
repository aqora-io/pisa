# Clients

pisa has clients for Rust, Python and TypeScript, and React hooks over the
TypeScript one. This page is for developers who call pisa from code: how to
install each client, what they share (versions, retries, jobs, errors), and
an example of each. The [API](api.md) describes the endpoints underneath.

## Installing

| Client | Package | Install | Notes |
| --- | --- | --- | --- |
| Rust | [`pisa-client`](https://crates.io/crates/pisa-client) | `cargo add pisa-client` | Async, on Tokio; [README](../crates/pisa-client/README.md) |
| Python | [`pisa-client`](https://pypi.org/project/pisa-client/) | `pip install pisa-client` | abi3 wheels for Python 3.10 or later; [README](../crates/pisa-python/README.md) |
| TypeScript | [`@aqora/pisa-client`](https://www.npmjs.com/package/@aqora/pisa-client) | `npm install @aqora/pisa-client` | No dependencies; browsers and Node; [README](../web/packages/client/README.md) |
| React | [`@aqora/pisa-react`](https://www.npmjs.com/package/@aqora/pisa-react) | `npm install @aqora/pisa-react @aqora/pisa-client @tanstack/react-query` | React 19.2 or later, TanStack Query v5; [README](../web/packages/react/README.md) |

The Rust client needs a Tokio runtime: `cargo add tokio --features
macros,rt-multi-thread`. Its `rustls` feature, on by default, gives HTTPS;
plain-HTTP deployments can turn it off. The Python client wraps the Rust one.
To build any of them from a clone, see [Development](development.md).

The examples below need no Mathlib: they run on a stack started with
`PROJECT=lean-core docker compose up --build` (see
[Running with Docker Compose](deploy-compose.md)).

## What the clients share

### Version, timeout and scope

Each client takes a default version and timeout, which its requests send
when a call names none:

| | Version | Timeout |
| --- | --- | --- |
| Rust | `Client::with_version` | `Client::with_timeout`, 60 s unless set |
| Python | `Client(url, version=...)` | `Client(url, timeout=60)` |
| TypeScript | `new PisaClient({ url, version })`, or per call | `timeout`, or per call; unset, the server's 60 s |

Without a version, a request runs on the server's default. Session, state,
document and snapshot ids carry their version, so the calls that take one
send none. A create that names a spec, with `replaces`, `amends` or
`refutes`, runs on that spec's version and doesn't send the client's. An
acceptance has a limit of its own, 600 s a pair, which a call can set
(`timeout`); it doesn't take the client's timeout.

A client may also take a [scope](api.md#scopes), which it sends with every
request as the `Pisa-Scope` header: `Client::with_scope` in Rust,
`Client(url, scope={...})` in Python and `scope` in TypeScript's options.
Runs are read and named by name with `get_run` and `name_run` (`getRun`,
`nameRun` in TypeScript); see [Naming a run](runs.md#naming-a-run).

### Retries

Every client sends a request again by itself, after a random wait that grows
each time, from 1 s to 30 s, and is at least the server's `Retry-After`: up
to 4 attempts within 5 minutes.

- **What is sent again.** A 429 is always sent again, since nothing ran. A
  request safe to send twice is also sent again after a 502, 503 or 504, or
  a connection that failed or dropped: reads, checks, searches, tactics,
  goals, probes, verifies, creates and dry runs. The Rust and Python clients
  also send any request again whose connection never opened. `/health` is
  never sent again: a draining server's 503 is its answer, which the clients
  return rather than raise.
- **Creates.** A create without a `key` gets one of the client's, `client:`
  and 32 hex digits, so a retry can't keep it twice; see
  [Retrying a create](snapshots.md#retrying-a-create).
- **Batches.** A batch's items that no worker took, or whose worker was
  lost, are sent again in a batch of their own.
- **Changing it.** `with_retry(RetryPolicy::none())` in Rust,
  `Client(url, retry=False)` in Python, and `retry: false` in TypeScript send
  each request once. Rust's `RetryPolicy { attempts, base_delay, max_delay,
  deadline }` and TypeScript's `retry: { attempts, baseDelayMs, maxDelayMs,
  deadlineMs }` change the policy.

### One call per kind of create

`POST /snapshots` is five things, by the fields it is sent with. Each has its
own call:

| Call | TypeScript | What it keeps |
| --- | --- | --- |
| `create_snapshot` | `createSnapshot` | Plain code |
| `create_spec` | `createSpec` | A spec, with its `holes` |
| `create_proof` | `createProof` | A proof that replaces a spec or a head of its run |
| `amend_spec` | `amendSpec` | A spec that amends another, with a reason |
| `refute` | `refute` | A refutation of a target of a spec |

On a `conflict` with one head, `create_proof` sends the same code again
replacing that head, with the same key, up to 3 times. It returns the last
answer, whose `refused` says what to do next. With `fork`, it starts a
branch and follows nothing. See [Replacing a snapshot](snapshots.md#replacing-a-snapshot)
and [Runs](runs.md).

### Long requests as jobs

A call that may run long has a twin that sends it as an
[async request](api.md#async-requests-and-jobs) and returns its job at once:
`submit_check`, `submit_start_session`, `submit_apply_tactic`,
`submit_check_session`, `submit_create_snapshot`, `submit_verify_snapshot`,
`submit_rebuild_snapshots`, `submit_settle_run`,
`submit_resubmit_snapshots` and `submit_accept_run` (`submitCheck` and so on
in TypeScript). Then:

- `wait_job` waits however long the job takes, looking every 25 s, and
  returns what the call would have, or raises what it would have raised;
- `poll_job` looks once, waiting up to the seconds it is given (at most 60);
- `cancel_job` withdraws a job no worker has started.

The server keeps a job's answer for an hour after the job is done, a day for
a snapshot's create and a run's acceptance. A dry run is answered at once,
never as a job: send it with the plain call. Anyone with a job's id can read
its answer or cancel it, so keep it secret.

### Errors

A check or a create reports its own failures in its result: a check that
couldn't complete has `error` set and `ok` false, and a create that kept
nothing has no `snapshot`, with `refused` and `check` saying why. A batch
does the same for each item. Every other failure is raised, with the API's
[error kind](api.md#error-kinds) and the HTTP status:

| Kind | Status | Rust | Python | TypeScript |
| --- | --- | --- | --- | --- |
| `lean`, `policy` | 422 | `Error::Server { kind, .. }` | `LeanError` | `isLeanError` |
| `timeout`, `out_of_memory`, `crash`, `recursion_limit`, `heartbeat_limit` | 422 | `Error::Server` | `LimitError` | `isLimitError` |
| `capacity` | 429 | `Error::Server` | `CapacityError` | `isCapacityError` |
| `unavailable`, `worker_lost` | 503 | `Error::Server` | `CapacityError` | `isCapacityError` |
| `bad_request` | 400 | `Error::Server` | `BadRequestError` | `isPisaError(e, "bad_request")` |
| `not_found` | 404 | `Error::Server` | `NotFoundError` | `isNotFoundError` |
| `conflict` | 409 | `Error::Server` | `ConflictError` | `isConflictError` |
| `internal` | 500 | `Error::Server` | `InternalError` | `isPisaError(e, "internal")` |
| No answer | | `Error::Http` | `PisaError`, `kind` None | `network`, or `aborted` |

- Every Python class is a `PisaError`, with `.kind`, `.status`, `.message`
  and `.resend_safe`. TypeScript throws a `PisaError` with `kind`, `status`
  and `resendSafe`. Rust's `Error::Server` has `status`, `kind`, `message`
  and `resend_safe`, and `Error::kind()` reads the kind of any error.
- `resend_safe` is set only for `worker_lost`: whether sending the request
  again is safe, as the server said; see
  [When a worker is lost](api.md#when-a-worker-is-lost).
- A gateway's 502, 503 or 504 whose body isn't pisa's is `unavailable` in
  every client. Any other answer that isn't pisa's is `internal` in Rust and
  Python, and `bad_response` in TypeScript.
- A request the client refuses before sending it, such as an audit against
  no snapshot, is `Error::Invalid` in Rust and `ValueError` in Python; an
  unusable URL is `Error::Url` and `ValueError`. TypeScript rejects such
  calls with `bad_request`.
- `lean`, `policy` and the limits mean the input itself failed, which during
  proof search is an ordinary dead end. `capacity`, `unavailable` and
  `worker_lost` reach the caller once the client's retries have run out:
  retry later.

## Rust

```rust
use std::collections::BTreeMap;

use pisa_client::{
    CheckOptions, Client, CreateSnapshotOptions, DeleteSnapshotOptions, Pos, RebuildOptions,
    Replaces,
};

#[tokio::main]
async fn main() -> Result<(), pisa_client::Error> {
    let client = Client::new("http://localhost:8000")?.with_timeout(60);
    let code = "theorem t : 2 + 2 = 4 := by decide";
    let result = client.check(code).await?;
    assert!(result.ok);
    // The tactic at line 1, column 28 (`decide`) and its goals; `Detail::Terms` in `detail` adds terms.
    let opts = CheckOptions { at: vec![Pos { line: 1, column: 28 }], ..Default::default() };
    let goals = client.check_with(code, &opts).await?.at;

    let session = client.start_session("theorem s (n : Nat) : n + 0 = n := by sorry").await?;
    let step = client.apply_tactic(&session.states[0].id, "simp").await?; // Error::Server { kind: Lean, .. } if it fails
    let verdict = client.check_session(&session.id, "theorem s (n : Nat) : n + 0 = n := by simp").await?;
    client.close_session(&session.id).await?;

    let run = BTreeMap::from([("run".to_string(), "7".to_string())]);
    let opts = CreateSnapshotOptions { labels: run.clone(), ..Default::default() };
    let base = client.create_snapshot_with("def double (n : Nat) := 2 * n", &opts).await?;
    let base = base.snapshot.expect("kept"); // none if nothing was kept: `refused` and `check` say why
    let next = client.check(&format!("{}\nexample : double 2 = 4 := rfl", base.header)).await?;
    let page = client.list_snapshots(&run, Some(50), None, false).await?; // newest first
    let more = client.list_snapshots(&run, Some(50), page.next.as_deref(), false).await?;
    let heads = client.list_snapshots(&run, None, None, true).await?; // only what nothing replaces
    // A merge patch: `Some` sets a label, `None` removes it, and the rest stay.
    let stage = BTreeMap::from([("stage".to_string(), Some("done".to_string()))]);
    let snapshot = client.update_snapshot(&base.id, &stage).await?;
    let verified = client.verify_snapshot(&base.id).await?;

    // Kept only if it keeps the replaced snapshot's statements.
    let replaces = Some(Replaces { snapshot_id: base.id.clone(), allow: None });
    let opts = CreateSnapshotOptions { replaces, labels: run.clone(), ..Default::default() };
    let lemma = "def double (n : Nat) := 2 * n\ntheorem double_two : double 2 = 4 := rfl";
    let extended = client.create_snapshot_with(lemma, &opts).await?.snapshot.expect("kept");
    let above = client.list_dependents(&base.id, &run).await?; // parents first
    let audit = client.audit_snapshots(&run).await?; // statuses, assumptions, what isn't proved
    let audit = client.audit_snapshots_against([&base.id], &[]).await?; // the run from base: clean, heads, targets
    // A plan: create each item, a level at a time, with its `replaces`.
    let opts = RebuildOptions { dry_run: true, ..Default::default() };
    let plan = client.rebuild_snapshots(&base.id, &extended.id, &run, &opts).await?.plan;
    // Or the server runs it; `retire: true` deletes the originals the copies replace, not `base`.
    let job = client.submit_rebuild_snapshots(&base.id, &extended.id, &run, &RebuildOptions::default()).await?;
    let rebuilt = client.wait_job(&job).await?; // rebuilt.created, rebuilt.failed
    let removed = client.delete_snapshot(&base.id).await?.removed; // the modules that went with it
    let opts = DeleteSnapshotOptions { with_replacers: true };
    let dropped = client.delete_snapshot_with(&extended.id, &opts).await?.deleted; // it and what replaces it

    // A long request as a job, answered however long it takes.
    let job = client.submit_check(code).await?;
    let result = client.wait_job(&job).await?; // what check would have returned
    Ok(())
}
```

- A call with options is the plain call's `_with` twin, e.g. `check_with`
  and `create_snapshot_with`, taking a `Default` struct: `CheckOptions`,
  `CreateSnapshotOptions`, `DeleteSnapshotOptions`, `AuditOptions`,
  `ExportOptions` and so on. The calls for specs, proofs, amendments,
  refutations, rebuilds, settles, resubmits and acceptances take theirs
  always.
- The API's request and response types are re-exported from
  [`pisa-protocol`](https://crates.io/crates/pisa-protocol).
- `Job::new(id, kind)` names a job submitted elsewhere, e.g. by another
  process, to wait for it.
- `Client` is cheap to clone, and its clones share connections.

## Python

```python
from pisa_client import Client, LeanError

client = Client("http://localhost:8000", timeout=60)
code = "theorem t : 2 + 2 = 4 := by decide"
client.check(code)["ok"]
client.check(code, at=[(1, 28)])["at"]   # decide's goals; detail=["terms"] adds terms
session = client.start_session("theorem s (n : Nat) : n + 0 = n := by sorry")
try:
    client.apply_tactic(session["states"][0]["id"], "exact foo")
except LeanError as e:
    print(e.kind, e.status, e.message)   # lean 422 ...
step = client.apply_tactic(session["states"][0]["id"], "simp")
client.check_session(session["id"], "theorem s (n : Nat) : n + 0 = n := by simp")["ok"]
client.close_session(session["id"])

base = client.create_snapshot("def double (n : Nat) := 2 * n", labels={"run": "7"})["snapshot"]
client.check(base["header"] + "\nexample : double 2 = 4 := rfl")["ok"]
page = client.list_snapshots({"run": "7"}, limit=50)   # newest first; pass page["next"] as after=
heads = client.list_snapshots({"run": "7"}, heads=True)   # only what nothing replaces
client.update_snapshot(base["id"], {"stage": "done", "draft": None})   # a merge patch: None removes a label
client.verify_snapshot(base["id"])["snapshot"]["verified"]
more = "def double (n : Nat) := 2 * n\ntheorem double_two : double 2 = 4 := rfl"
extended = client.create_snapshot(more, replaces={"snapshot_id": base["id"]}, labels={"run": "7"})["snapshot"]   # "allow": {"definitions": ["double"]} lets double change
client.list_dependents(base["id"], {"run": "7"})["snapshots"]   # parents first
client.audit_snapshots({"run": "7"})["summary"]   # by status: proved, conditional, sorry, rejected
client.audit_snapshots(against=[base["id"]])["clean"]   # the run from base, to its heads
plan = client.rebuild_snapshots(base["id"], extended["id"], {"run": "7"}, dry_run=True)["plan"]   # create each item, a level at a time, with its replaces
done = client.wait_job(client.submit_rebuild_snapshots(base["id"], extended["id"], {"run": "7"}))   # retire=True deletes the originals the copies replace, not base
client.delete_snapshot(base["id"])["removed"]   # the modules that went with it
client.delete_snapshot(extended["id"], with_replacers=True)["deleted"]   # it and what replaces it
job = client.submit_check(code)
client.wait_job(job)["ok"]   # what check would have returned, or raised
```

`Client`'s calls block. They release the GIL, so one `Client` can be shared
across threads, and they stop at Ctrl-C, dropping the request. `AsyncClient`
takes the same arguments, and its calls return asyncio futures instead,
which start at once; cancelling one drops its request. Either way, a job a
worker took runs on.

```python
import asyncio
from pisa_client import AsyncClient

async def main(codes):
    client = AsyncClient("http://localhost:8000", timeout=60)
    return await asyncio.gather(*(client.check(code) for code in codes))
```

Results are plain dicts; the type stubs (`_native.pyi`) describe their
shapes as `TypedDict`s and document every call. The exception classes, all
`PisaError`s, say what to do:

- `LeanError`: Lean or the policy refused the input.
- `LimitError`: the input hit a timeout, the memory limit, Lean's recursion
  or heartbeat limit (`recursion_limit`, `heartbeat_limit`), or crashed Lean.
- `CapacityError`: no worker was free, the server is unavailable, the job
  was lost (`worker_lost`), or a gateway answered 502, 503 or 504, once the
  client's retries ran out; retry later.
- `NotFoundError`: unknown or expired job, state or session, or unknown
  snapshot or declaration.
- `BadRequestError`: the request is malformed, e.g. an invalid id or a
  parameter that doesn't fit the others; sent again as it is, it fails again.
- `ConflictError`: the request doesn't fit what it found, e.g. a document at
  another revision, or a run name another run holds.
- `InternalError`: the server failed.
- `PisaError`: the base class; also raised for transport failures, with no
  `kind` or `status`.

## TypeScript

```ts
import { PisaClient, isLeanError } from "@aqora/pisa-client";

const client = new PisaClient({ url: "http://localhost:8000", timeout: 60 });
const result = await client.check("theorem t : 2 + 2 = 4 := by decide");
const session = await client.startSession("theorem s (n : Nat) : n + 0 = n := by sorry");
try {
  await client.applyTactic(session.states[0]!.id, "exact foo");
} catch (e) {
  if (!isLeanError(e)) throw e; // a PisaError of kind "lean", with Lean's errors
}
const step = await client.applyTactic(session.states[0]!.id, "simp");
await client.closeSession(session.id);
const { snapshot } = await client.createSnapshot("def double (n : Nat) := 2 * n");
await client.check(`${snapshot!.header}\nexample : double 2 = 4 := rfl`);
```

Every method takes a last options argument with a `signal`; aborting gives
the job up only if no worker has claimed it yet. The
[package README](../web/packages/client/README.md) lists every method by
area, with a longer example, and the helpers for positions (`posMapper`,
`rangeOf`) and session proofs (`proofText`).

## React

`@aqora/pisa-react` has hooks over TanStack Query:

- `useCheck`, `useLiveCheck` and `useCheckBatch` for checks;
- `useProofSession` for a tree of tactics, which closes its session when
  reset or unmounted;
- `useDocument` with `useDocumentGoals` and `useProbeDocument` for open
  documents;
- `useSnapshots`, `useSnapshot`, `useCreateSnapshot`, `useUpdateSnapshot`,
  `useDeleteSnapshot`, `useVerifySnapshot`, `useDependents` and `useAudit`
  for snapshots;
- `useLongJob` for requests run as jobs;
- `useRebuildPlan`, `useRebuildSnapshots`, `useAcceptRun`, `useAcceptances`
  and `useExportRun` for runs;
- `useSearch`, `useResolve` and `useDeclaration` for search, and `useHealth`.

```tsx
import { PisaClient } from "@aqora/pisa-client";
import { PisaProvider, useLiveCheck } from "@aqora/pisa-react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const client = new PisaClient({ url: "/api" });

export function Root() {
  return (
    <QueryClientProvider client={new QueryClient()}>
      <PisaProvider client={client}>
        <Editor code="theorem t : 2 + 2 = 4 := by decide" />
      </PisaProvider>
    </QueryClientProvider>
  );
}

function Editor({ code }: { code: string }) {
  const check = useLiveCheck(code); // check.data: { code, result }
  return <p>{check.data?.result.ok ? "ok" : "not yet"}</p>;
}
```

Environments, run names, settling, resubmitting, flagging a description and
reading one acceptance by id have no hook: `usePisaClient()` gives the
provider's client to call them. The
[package README](../web/packages/react/README.md) describes each hook, the
query option factories (`checkOptions`, `searchOptions` and the like) and
`retryQuery`.
