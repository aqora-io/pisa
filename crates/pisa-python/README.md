# pisa-client

A Python client for [pisa](https://github.com/aqora-io/pisa), a server that
checks Lean 4 code. It checks code, runs proof sessions, keeps accepted code
as snapshots that later code imports, judges runs against their specs, and
searches declarations. It wraps pisa's Rust client, and ships type stubs.

```sh
pip install pisa-client
```

```python
from pisa_client import Client, LeanError

client = Client("http://localhost:8000", timeout=60)
assert client.check("theorem t : 2 + 2 = 4 := rfl")["ok"]

session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry")
try:
    client.apply_tactic(session["states"][0]["id"], "exact foo")
except LeanError as e:
    print(e.kind, e.message)
step = client.apply_tactic(session["states"][0]["id"], "simp")
assert step["complete"]
client.close_session(session["id"])
```

- `Client`'s calls block, with the GIL released, and stop at Ctrl-C.
  `AsyncClient` takes the same arguments, and its calls return asyncio
  futures.
- Responses are dicts, typed as `TypedDict`s in the stubs.
- `version` names the environment the requests run in, `timeout` is Lean's
  time limit in seconds, and `scope` sends every request in a tenancy scope.
- Each long call has a `submit_*` twin that queues it as a job. `wait_job`
  and `poll_job` follow the job.
- Errors are `PisaError`s, with the API's `kind` and the HTTP `status`. The
  subclasses group the kinds: `LeanError`, `LimitError`, `CapacityError`,
  `NotFoundError`, `BadRequestError`, `ConflictError` and `InternalError`.

Wheels need Python 3.10 or later. The
[API documentation](https://github.com/aqora-io/pisa/tree/main/docs)
describes every endpoint.

Licensed under either of MIT or Apache-2.0, at your option.
