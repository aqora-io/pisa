# pisa-client

An async Rust client for [pisa](https://github.com/aqora-io/pisa), a server
that checks Lean 4 code. It checks code, runs proof sessions, keeps accepted
code as snapshots that later code imports, judges runs against their specs,
and searches declarations.

```toml
[dependencies]
pisa-client = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), pisa_client::Error> {
    let client = pisa_client::Client::new("http://localhost:8000")?.with_timeout(60);

    let result = client.check("theorem t : 2 + 2 = 4 := rfl").await?;
    assert!(result.ok);

    let session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry").await?;
    let step = client.apply_tactic(&session.states[0].id, "simp").await?;
    assert!(step.complete);
    client.close_session(&session.id).await?;
    Ok(())
}
```

- `with_version` names the environment the requests run in; without it, the
  server's default.
- `with_timeout` sets Lean's time limit in seconds for each request.
- `with_retry` sets how the client sends a request again after a 503, a
  network error or a lost job. It only resends a request that is safe to
  send twice.
- Each long call has a `submit_*` twin that queues it as a job. `wait_job`
  and `poll_job` follow the job.
- A failure in the input itself, such as Lean's error or a timeout, is an
  `Error::Server` whose `kind` says which.

The `rustls` feature (on by default) gives HTTPS; turn it off for plain
HTTP.

The request and response types are in
[`pisa-protocol`](https://crates.io/crates/pisa-protocol), and re-exported
here. The [API documentation](https://github.com/aqora-io/pisa/tree/main/docs)
describes every endpoint.

Licensed under either of MIT or Apache-2.0, at your option.
