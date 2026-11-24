//! Amending a spec: a create that names the spec it amends is a spec of its
//! own, and records what changed of the amended spec, what it adds, and why.
//! The fake Lean's "type hash" is a declaration's text between its name and
//! `:=`, and a `def`'s "value hash" the text after it.

mod common;

use common::{Cluster, create};
use serde_json::{Value, json};

const SPEC: &str = "import Foo\n\ndef cost : Nat := sorry\ntheorem t : cost = 1 := sorry";
const AMENDED: &str = "import Foo\n\ndef cost : Nat := sorry\ntheorem t : cost = 2 := sorry\ntheorem u : True := sorry";

fn id(r: &Value) -> String {
    r["snapshot"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no snapshot: {r}"))
        .to_string()
}

/// The entry id in a snapshot id.
fn bare(id: &str) -> &str {
    id.split_once('~').unwrap().1
}

fn amends(id: &str, reason: &str) -> Value {
    json!({"snapshot_id": id, "reason": reason})
}

async fn delete(c: &Cluster, id: &str) {
    let r = c.client.delete(c.url(&format!("/snapshots/{id}"))).send();
    assert_eq!(r.await.unwrap().status(), 200);
}

/// The amendments stored, by the entry that amends.
async fn amendments(c: &Cluster) -> Vec<String> {
    let rows = c.stored_rows("v").await;
    rows.into_iter()
        .filter(|r| r[0] == "amendments")
        .map(|r| r[1].clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_amendment_records_what_changed_what_it_adds_and_why() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": SPEC, "holes": ["cost"]})).await);
    let body = json!({"code": AMENDED, "holes": ["cost"], "amends": amends(&old, "t was false")});
    let r = create(&c, body.clone()).await;
    let new = id(&r);
    let amendment = &r["amendment"];
    assert_eq!(
        (&amendment["snapshot_id"], &amendment["amends"]),
        (&json!(new), &json!(old)),
        "{r}"
    );
    assert_eq!(amendment["reason"], "t was false");
    assert_eq!(
        (
            &amendment["changes"][0]["name"],
            &amendment["changes"][0]["change"]
        ),
        (&json!("t"), &json!("type")),
        "{r}"
    );
    assert_eq!(amendment["changes"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(amendment["added"], json!(["u"]));
    // A spec of its own, re-checked, that replaces nothing.
    let s = &r["snapshot"];
    assert_eq!(
        (&s["holes"], &s["verified"]),
        (&json!(["cost"]), &json!(true))
    );
    assert!(s.get("replaces").is_none(), "{r}");
    // A spec is amended once: a second amendment names the current one.
    let twice = create(&c, json!({"code": SPEC, "amends": amends(&old, "again")})).await;
    assert!(twice.get("snapshot").is_none(), "{twice}");
    assert_eq!(twice["check"]["error"]["kind"], "conflict", "{twice}");
    let message = twice["check"]["error"]["message"].as_str().unwrap();
    assert!(message.contains(&new), "{message}");
    // A key replay answers it again, and so does the same code, kept.
    let keyed = json!({"key": "amend-1", "code": SPEC, "amends": amends(&new, "again")});
    let first = create(&c, keyed.clone()).await;
    let again = create(&c, keyed).await;
    assert_eq!(again["amendment"], first["amendment"]);
    assert_eq!(first["amendment"]["reason"], "again");
    let first = id(&first);
    let same = create(
        &c,
        json!({"code": SPEC, "amends": amends(&first, "nothing")}),
    )
    .await;
    assert_eq!(
        (&same["amendment"]["changes"], &same["amendment"]["added"]),
        (&json!([]), &json!([])),
        "{same}"
    );
    let same = id(&same);

    // The record goes with the spec that amends, and outlives the amended.
    delete(&c, &same).await;
    delete(&c, &first).await;
    assert_eq!(amendments(&c).await, [bare(&new)]);
    delete(&c, &old).await;
    assert_eq!(amendments(&c).await, [bare(&new)]);
    delete(&c, &new).await;
    assert!(amendments(&c).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_amendment_is_a_re_checked_spec_with_a_reason() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": SPEC})).await);
    let long = "x".repeat(4097);
    for (body, says) in [
        (
            json!({"code": AMENDED, "amends": amends(&old, "r"), "replaces": {"snapshot_id": old}}),
            "replaces nothing",
        ),
        (
            json!({"code": AMENDED, "amends": amends(&old, "r"), "verify": false}),
            "always re-checked",
        ),
        (
            json!({"code": AMENDED, "amends": amends(&old, " ")}),
            "1 to 4096",
        ),
        (
            json!({"code": AMENDED, "amends": amends(&old, &long)}),
            "1 to 4096",
        ),
        (
            json!({"code": AMENDED, "amends": amends("nope", "r")}),
            "not a snapshot id",
        ),
        (
            json!({"code": AMENDED, "version": "w", "amends": amends(&old, "r")}),
            "of version",
        ),
    ] {
        let (status, r) = c.post("/snapshots", body.clone()).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{body}: {r}"
        );
        let message = r["error"]["message"].as_str().unwrap();
        assert!(message.contains(says), "{message}");
    }
    // A spec that isn't there keeps nothing.
    let unknown = format!("{}~{}", c.version("v"), "0".repeat(32));
    let r = create(
        &c,
        json!({"code": AMENDED, "amends": amends(&unknown, "r")}),
    )
    .await;
    assert_eq!(r["check"]["error"]["kind"], "not_found", "{r}");
    assert!(r.get("snapshot").is_none(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_audit_lists_a_specs_amendments_both_ways() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": SPEC, "holes": ["cost"]})).await);
    let proof = "import Foo\n\ndef cost : Nat := 1\ntheorem t : cost = 1 := rfl";
    let replaces = json!({"snapshot_id": old});
    create(&c, json!({"code": proof, "replaces": replaces})).await;
    let audit = |spec: String| {
        let c = &c;
        async move {
            let r = c
                .client
                .get(c.url(&format!("/snapshots/audit?against={spec}")));
            let r = r.send().await.unwrap();
            assert_eq!(r.status(), 200);
            r.json::<Value>().await.unwrap()
        }
    };
    let before = audit(old.clone()).await;
    assert!(before.get("amendments").is_none(), "{before}");
    let body = json!({"code": AMENDED, "holes": ["cost"], "amends": amends(&old, "t was false")});
    let amendment = create(&c, body).await["amendment"].clone();
    let new = amendment["snapshot_id"].as_str().unwrap().to_string();
    // From the amended spec, which it supersedes, and from the new.
    assert_eq!(before["clean"], true, "{before}");
    let after = audit(old.clone()).await;
    assert_eq!(after["amendments"], json!([amendment]));
    assert_eq!(
        after["superseded"],
        json!([{"spec": old, "by": new}]),
        "{after}"
    );
    assert_eq!(after["clean"], false, "{after}");
    let current = audit(new).await;
    assert_eq!(current["amendments"], json!([amendment]));
    assert!(current.get("superseded").is_none(), "{current}");
}

/// A spec with a false target `w`, and its amendment without it.
const WITH_W: &str = "import Foo\n\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry\ntheorem w : 1 = 2 := sorry";
const WITHOUT_W: &str = "import Foo\n\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry";

/// A head of `WITH_W` that fills `cost` with `n`.
fn head(n: u32) -> String {
    format!(
        "import Foo\n\ndef cost : Nat := {n}\ntheorem t : cost = cost := rfl\ntheorem w : 1 = 2 := sorry"
    )
}

async fn resubmit(c: &Cluster, body: Value) -> (u16, Value) {
    c.post("/snapshots/resubmit", body).await
}

async fn audit_against(c: &Cluster, spec: &str) -> Value {
    let r = c
        .client
        .get(c.url(&format!("/snapshots/audit?against={spec}")));
    let r = r.send().await.unwrap();
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resubmit_carries_a_run_over_to_the_amending_spec() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": WITH_W, "holes": ["cost"]})).await);
    let replaces = json!({"snapshot_id": old});
    let labels = json!({"run": "r1"});
    let h = create(
        &c,
        json!({"code": head(1), "replaces": replaces, "labels": labels}),
    )
    .await;
    let body = json!({"code": WITHOUT_W, "holes": ["cost"], "amends": amends(&old, "w is false")});
    let new = id(&create(&c, body).await);

    let ask = json!({"from": old, "to": new, "dry_run": true});
    let (status, plan) = resubmit(&c, ask).await;
    assert_eq!(status, 200, "{plan}");
    assert_eq!(plan["ok"], true, "{plan}");
    let item = &plan["items"][0];
    assert_eq!(
        (&item["snapshot_id"], &item["passes"]),
        (&h["snapshot"]["id"], &json!(true)),
        "{plan}"
    );
    assert!(item.get("new_snapshot_id").is_none(), "{plan}");
    assert_eq!(item["replacement"]["changed_definitions"], json!(["cost"]));

    let (status, done) = resubmit(&c, json!({"from": old, "to": new})).await;
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["ok"], true, "{done}");
    let made = done["items"][0]["new_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    let audit = audit_against(&c, &new).await;
    assert_eq!(audit["heads"], json!([made]), "{audit}");
    // It keeps the new spec's statements (the fake marks every theorem of
    // code with a `sorry` as not proved, so the run isn't clean).
    assert!(audit["comparisons"][0].get("reasons").is_none(), "{audit}");
    // The same code, with its head's labels.
    let copy = c.client.get(c.url(&format!("/snapshots/{made}"))).send();
    let copy: Value = copy.await.unwrap().json().await.unwrap();
    assert_eq!(
        (&copy["module"], &copy["labels"]),
        (&h["snapshot"]["module"], &labels)
    );
    // Sent again, as a job too, it replays.
    let (_, again) = resubmit(&c, json!({"from": old, "to": new})).await;
    assert_eq!(again, done);
    let r = c
        .client
        .post(c.url("/snapshots/resubmit"))
        .header("prefer", "respond-async")
        .json(&json!({"from": old, "to": new}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let job: Value = r.json().await.unwrap();
    assert_eq!(job["kind"], "resubmit_snapshots");
    let r = c
        .client
        .get(c.url(&format!("/jobs/{}?wait=30", job["id"].as_str().unwrap())));
    let r = r.send().await.unwrap();
    let status = r.status().as_u16();
    let answer = common::job_answer(status, r.json::<Value>().await.unwrap());
    assert_eq!(answer, (200, done));
    // The amended spec may be gone.
    delete(&c, &old).await;
    let (_, after) = resubmit(&c, json!({"from": old, "to": new, "dry_run": true})).await;
    assert_eq!(
        after["items"][0]["snapshot_id"], h["snapshot"]["id"],
        "{after}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forked_run_comes_over_forked() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": WITH_W, "holes": ["cost"]})).await);
    for (n, fork) in [(1, false), (2, true)] {
        let body = json!({"code": head(n), "replaces": {"snapshot_id": old}, "fork": fork});
        create(&c, body).await;
    }
    let body = json!({"code": WITHOUT_W, "holes": ["cost"], "amends": amends(&old, "w is false")});
    let new = id(&create(&c, body).await);
    let (_, done) = resubmit(&c, json!({"from": old, "to": new})).await;
    assert_eq!(done["ok"], true, "{done}");
    let audit = audit_against(&c, &new).await;
    assert_eq!(audit["heads"].as_array().unwrap().len(), 2, "{audit}");
    assert_eq!(audit["forks"][0]["snapshot_id"], json!(new), "{audit}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_head_that_changes_the_amended_statements_stays() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": WITH_W, "holes": ["cost"]})).await);
    let replaces = json!({"snapshot_id": old});
    let h = create(&c, json!({"code": head(1), "replaces": replaces})).await;
    // A head whose record the kernel hasn't re-checked.
    let body = json!({"code": head(2), "replaces": replaces, "fork": true, "verify": false});
    let unchecked = create(&c, body).await;
    let changed = WITHOUT_W.replace("cost = cost", "cost = cost + 0");
    let body = json!({"code": changed, "holes": ["cost"], "amends": amends(&old, "t was weak")});
    let new = id(&create(&c, body).await);
    let (status, done) = resubmit(&c, json!({"from": old, "to": new})).await;
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["ok"], false, "{done}");
    let items = done["items"].as_array().unwrap();
    assert_eq!(items[0]["snapshot_id"], h["snapshot"]["id"]);
    assert_eq!(items[0]["passes"], false);
    assert_eq!(
        items[0]["reasons"],
        json!(["statement: t has a different type"]),
        "{done}"
    );
    assert_eq!(items[1]["snapshot_id"], unchecked["snapshot"]["id"]);
    let reason = items[1]["reasons"][0].as_str().unwrap();
    assert!(reason.contains("isn't verified"), "{reason}");
    assert!(items.iter().all(|i| i.get("new_snapshot_id").is_none()));
    assert!(audit_against(&c, &new).await["heads"] == json!([new]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resubmit_is_from_a_spec_to_one_that_amends_it() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": WITH_W, "holes": ["cost"]})).await);
    let other = id(&create(&c, json!({"code": WITHOUT_W})).await);
    let elsewhere = format!("w~{}", bare(&other));
    for (body, says) in [
        (json!({"from": old, "to": other}), "doesn't amend"),
        (json!({"from": old, "to": elsewhere}), "of version"),
        (json!({"from": "nope", "to": other}), "not a snapshot id"),
    ] {
        let (status, r) = resubmit(&c, body.clone()).await;
        assert_eq!(status, 400, "{body}: {r}");
        let message = r["error"]["message"].as_str().unwrap();
        assert!(message.contains(says), "{message}");
    }
}

/// Asserts that a create into a run kept nothing: a conflict that names
/// `current` and gives its heads, `heads`.
fn superseded(r: &Value, current: &str, heads: &[String]) {
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(r["check"]["error"]["kind"], "conflict", "{r}");
    let message = r["check"]["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("amended by {current}")),
        "{message}"
    );
    assert_eq!(r["heads"], json!(heads), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_whose_spec_is_amended_takes_no_more_submissions() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": WITH_W, "holes": ["cost"]})).await);
    let h = id(&create(
        &c,
        json!({"code": head(1), "replaces": {"snapshot_id": old}}),
    )
    .await);
    let body = json!({"code": WITHOUT_W, "holes": ["cost"], "amends": amends(&old, "w is false")});
    let new = id(&create(&c, body).await);
    // Into the old run, onto its head or its spec, forked or not.
    for (replaced, fork) in [(&h, false), (&h, true), (&old, true)] {
        let body = json!({"code": head(2), "replaces": {"snapshot_id": replaced}, "fork": fork});
        superseded(&create(&c, body).await, &new, std::slice::from_ref(&new));
    }
    // Once the run is carried over, the conflict gives its heads.
    let (_, done) = resubmit(&c, json!({"from": old, "to": new})).await;
    let made = done["items"][0]["new_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    let body = json!({"code": head(2), "replaces": {"snapshot_id": h}});
    superseded(&create(&c, body).await, &new, &[made]);
    // An amendment of the amendment supersedes both.
    let newer = WITHOUT_W.replace("cost = cost", "cost + 0 = cost");
    let body = json!({"code": newer, "holes": ["cost"], "amends": amends(&new, "simpler")});
    let newer = id(&create(&c, body).await);
    let body = json!({"code": head(2), "replaces": {"snapshot_id": h}});
    superseded(
        &create(&c, body).await,
        &newer,
        std::slice::from_ref(&newer),
    );
    let audit = audit_against(&c, &old).await;
    assert_eq!(
        audit["superseded"],
        json!([{"spec": old, "by": newer}]),
        "{audit}"
    );
    // The current spec's run takes them.
    let body = json!({"code": WITHOUT_W.replace("sorry", "1"), "replaces": {"snapshot_id": newer}});
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert_ne!(r["check"]["error"]["kind"], "conflict", "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_amendment_keeps_the_holes_it_doesnt_change() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": WITH_W, "holes": ["cost"]})).await);
    // Naming none keeps the amended spec's.
    let r = create(
        &c,
        json!({"code": WITHOUT_W, "amends": amends(&old, "w is false")}),
    )
    .await;
    assert_eq!(r["snapshot"]["holes"], json!(["cost"]), "{r}");
    assert!(r["amendment"].get("holes").is_none(), "{r}");
    // Naming some replaces them, and the record has both.
    let first = id(&r);
    let two = "import Foo\n\ndef cost : Nat := sorry\ndef rate : Nat := sorry\ntheorem t : cost = cost := sorry";
    let body =
        json!({"code": two, "holes": ["cost", "rate"], "amends": amends(&first, "rate too")});
    let r = create(&c, body).await;
    assert_eq!(r["snapshot"]["holes"], json!(["cost", "rate"]), "{r}");
    let changed = json!({"before": ["cost"], "after": ["cost", "rate"]});
    assert_eq!(r["amendment"]["holes"], changed, "{r}");
    // An empty list, with `amends` only, leaves none.
    let second = id(&r);
    let fixed = "import Foo\n\ndef cost : Nat := 3\ntheorem t : cost = cost := sorry";
    let body = json!({"code": fixed, "holes": [], "amends": amends(&second, "cost is 3")});
    let r = create(&c, body).await;
    assert!(r["snapshot"].get("holes").is_none(), "{r}");
    assert_eq!(
        r["amendment"]["holes"],
        json!({"before": ["cost", "rate"], "after": []}),
        "{r}"
    );
    let (status, r) = c
        .post("/snapshots", json!({"code": fixed, "holes": []}))
        .await;
    assert_eq!(
        (status, &r["error"]["kind"]),
        (400, &json!("bad_request")),
        "{r}"
    );
    // An audit lists the record as the create answered it.
    let audit = audit_against(&c, &first).await;
    let recorded: Vec<&Value> = audit["amendments"].as_array().unwrap().iter().collect();
    assert!(recorded.iter().any(|a| a["holes"] == changed), "{audit}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resubmit_comes_over_forked_onto_a_spec_replaced_already() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = id(&create(&c, json!({"code": WITH_W, "holes": ["cost"]})).await);
    create(
        &c,
        json!({"code": head(1), "replaces": {"snapshot_id": old}}),
    )
    .await;
    let body = json!({"code": WITHOUT_W, "holes": ["cost"], "amends": amends(&old, "w is false")});
    let new = id(&create(&c, body).await);
    // An agent submits to the new spec before the run is carried over.
    let theirs = WITHOUT_W.replace("cost : Nat := sorry", "cost : Nat := 2");
    let theirs = id(&create(
        &c,
        json!({"code": theirs, "replaces": {"snapshot_id": new}}),
    )
    .await);
    let (status, done) = resubmit(&c, json!({"from": old, "to": new})).await;
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["ok"], true, "{done}");
    let made = done["items"][0]["new_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    let audit = audit_against(&c, &new).await;
    let heads: Vec<&Value> = audit["heads"].as_array().unwrap().iter().collect();
    assert!(
        heads.contains(&&json!(theirs)) && heads.contains(&&json!(made)),
        "{audit}"
    );
    // Sent again, it replays.
    let (_, again) = resubmit(&c, json!({"from": old, "to": new})).await;
    assert_eq!(again, done);
}
