//! Statuses and the audit: an axiom that snapshot code declares is an
//! assumption, so what rests only on assumptions is `conditional`, apart
//! from `sorry` and what is `rejected`. fake-lean's theorems use every axiom
//! their own code declares or uses, and the imported ones their line names.

mod common;

use common::Cluster;
use serde_json::{Value, json};

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert!(r["entry"].is_string(), "{r}");
    r
}

/// `header`, a blank line, then `body`.
fn on(snapshot: &Value, body: &str) -> String {
    format!("{}\n\n{body}", snapshot["header"].as_str().unwrap())
}

async fn get(c: &Cluster, path: &str) -> (u16, Value) {
    let r = c.client.get(c.url(path)).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

fn decl<'a>(result: &'a Value, name: &str) -> &'a Value {
    let decls = result["declarations"].as_array().unwrap();
    decls
        .iter()
        .find(|d| d["name"] == name)
        .unwrap_or_else(|| panic!("no {name}: {result}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn statuses_tell_assumptions_from_sorry_and_the_rest() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    // The assumption, outside the project's labels.
    let a = create(&c, json!({"code": "import Foo\n\naxiom kunneth : 1 = 1"})).await;
    let p1 = json!({"p": "1"});
    let code = on(
        &a,
        "theorem thm3 : 1 = 1 := kunneth\ntheorem cor1 : 2 = 2 := kunneth",
    );
    let b = create(&c, json!({"code": code, "labels": p1})).await;
    let lem = create(
        &c,
        json!({"code": on(&a, "theorem lem2 : 1 = 1 := sorry"), "labels": p1}),
    )
    .await;
    let nd = create(
        &c,
        json!({"code": on(&a, "theorem nd : 1 = 1 := by native_decide"), "labels": p1}),
    )
    .await;

    assert_eq!(
        (&a["status"], &a["dirty"]),
        (&json!("conditional"), &json!(true))
    );
    let thm3 = decl(&b["result"], "thm3");
    assert_eq!(
        (&thm3["status"], &thm3["assumes"]),
        (&json!("conditional"), &json!(["kunneth"]))
    );
    assert_eq!(
        (&b["status"], &b["dirty"]),
        (&json!("conditional"), &json!(true)),
        "{b}"
    );
    assert_eq!(lem["status"], "sorry");
    assert_eq!(nd["status"], "rejected");
    let (_, listed) = get(&c, "/snapshots?label=p:1").await;
    let statuses: Vec<&Value> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["status"])
        .collect();
    assert_eq!(
        statuses,
        [&json!("rejected"), &json!("sorry"), &json!("conditional")]
    );

    // A check on top sees the assumption too, and `ok` is as it was.
    let (_, checked) = c
        .post(
            "/check",
            json!({"code": on(&b, "theorem t : 3 = 3 := kunneth")}),
        )
        .await;
    let t = decl(&checked, "t");
    assert_eq!(
        (&t["status"], &t["assumes"], &checked["ok"]),
        (&json!("conditional"), &json!(["kunneth"]), &json!(false))
    );

    let (status, audit) = get(&c, "/snapshots/audit?label=p:1").await;
    assert_eq!(status, 200, "{audit}");
    assert_eq!(
        audit["summary"],
        json!({"proved": 0, "conditional": 2, "sorry": 1, "rejected": 1, "unverified_modules": 3})
    );
    let assumptions = audit["assumptions"].as_array().unwrap();
    assert_eq!(assumptions.len(), 1, "{audit}");
    let kunneth = &assumptions[0];
    assert_eq!(
        (&kunneth["name"], &kunneth["entry"]),
        (&json!("kunneth"), &a["entry"])
    );
    assert_eq!(kunneth["used_by"], json!(["cor1", "thm3"]));
    assert_eq!(
        (&kunneth["verified"], kunneth["signature"].is_string()),
        (&json!(false), true)
    );
    let mut not_proved: Vec<(String, String, Value)> = audit["not_proved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["name"].as_str().unwrap().into(),
                d["status"].as_str().unwrap().into(),
                d["axioms"].clone(),
            )
        })
        .collect();
    not_proved.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(
        not_proved,
        [
            ("lem2".into(), "sorry".into(), json!(["sorryAx"])),
            ("nd".into(), "rejected".into(), json!(["Lean.ofReduceBool"])),
        ]
    );

    // Verifying B re-checks it and what it imports.
    let path = format!("/snapshots/{}/verify", b["entry"].as_str().unwrap());
    let (status, verified) = c.post(&path, json!({})).await;
    assert_eq!(
        (status, &verified["verified"]),
        (200, &json!(true)),
        "{verified}"
    );
    let (_, audit) = get(&c, "/snapshots/audit?label=p:1").await;
    assert_eq!(audit["summary"]["unverified_modules"], 2);
    assert_eq!(audit["assumptions"][0]["verified"], true);
}
