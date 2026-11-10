//! A stand-in for the `pisa` tool (`docker/pisa-lean`), for tests.
//!
//! `pisa verify M1 … Mk` re-verifies snapshot modules, imports first: it
//! prints `PISA_IMPORTED {}`, then for each module, whose own code it reads
//! from its fake olean on LEAN_PATH, a `PISA_VERIFY` line with the module's
//! declarations (see `pisa_tests::declarations`). A `forge` marker in the code
//! stands in for the kernel catching a forged declaration: the module is
//! rejected and the run ends. A `SLOW_KERNEL <ms>` marker makes the module
//! take that long.

use std::io::Write;

use pisa_tests::{declarations, find_module};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("verify") => verify(&args[1..]),
        _ => {
            eprintln!("usage: pisa verify MODULE…");
            std::process::exit(2);
        }
    }
}

fn verify(modules: &[String]) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "PISA_IMPORTED {{}}").unwrap();
    out.flush().unwrap();
    for module in modules {
        let code = find_module(module)
            .and_then(|f| std::fs::read_to_string(f).ok())
            .unwrap_or_default();
        if let Some(ms) = code.split("SLOW_KERNEL ").nth(1) {
            let ms: u64 = ms.lines().next().unwrap().trim().parse().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
        let rejected = code.contains("forge");
        let verdict = if rejected {
            json!({"module": module, "ok": false, "reason": "kernel re-check: forged declaration"})
        } else {
            let decls: serde_json::Value = serde_json::from_str(&declarations(&code)).unwrap();
            json!({"module": module, "ok": true, "declarations": decls})
        };
        writeln!(out, "PISA_VERIFY {verdict}").unwrap();
        out.flush().unwrap();
        if rejected {
            return;
        }
    }
}
