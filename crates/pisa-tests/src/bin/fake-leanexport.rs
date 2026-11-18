//! A stand-in for Lean's bundled `leanexport`, for tests.
//!
//! `leanexport <Module> -- <targets…>` finds the module on `LEAN_PATH`, as
//! Lean does, and prints its "export": one JSON line with the module, the
//! targets, in the order given, and its code, which is what a fake olean
//! holds. Like the worker's sandbox expects of the real one, it refuses to
//! run (exit 3) unless `LEAN_SYSROOT` is set, so that it never looks for
//! `lean` on `PATH`, and `LEAN_ABORT_ON_PANIC` is `1`, so that a constant it
//! can't find fails it instead of leaving it out. A module that says
//! `EXPORT_PANIC` panics as a missing constant does (exit 134), and one that
//! says `SLOW_EXPORT <ms>` takes that long.

use std::process::exit;

use pisa_tests::find_module;
use serde_json::json;

fn main() {
    let sysroot = std::env::var_os("LEAN_SYSROOT").is_some();
    let abort = std::env::var("LEAN_ABORT_ON_PANIC").as_deref() == Ok("1");
    if !(sysroot && abort) {
        eprintln!("fake-leanexport: run without LEAN_SYSROOT and LEAN_ABORT_ON_PANIC=1");
        exit(3);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (module, targets) = match args.split_first() {
        Some((module, rest)) if rest.first().map(String::as_str) == Some("--") => {
            (module, &rest[1..])
        }
        _ => {
            eprintln!("usage: leanexport <Module> -- <targets…>");
            exit(2);
        }
    };
    let Some(code) = find_module(module).and_then(|f| std::fs::read_to_string(f).ok()) else {
        eprintln!("uncaught exception: unknown module {module}");
        exit(1);
    };
    if let Some(ms) = code.split("SLOW_EXPORT ").nth(1) {
        let ms: u64 = ms.lines().next().unwrap().trim().parse().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    if code.contains("EXPORT_PANIC") {
        eprintln!(
            "PANIC at LeanExport.dumpConstant LeanExport.Basic:254:48: Constant t not found in \
             environment."
        );
        exit(134);
    }
    println!(
        "{}",
        json!({"module": module, "targets": targets, "code": code})
    );
}
