//! A stand-in for the `pisa` tool (`docker/pisa-lean`), for tests.
//!
//! `pisa build -R <root> -o <olean> … <file>` compiles a module: the "olean"
//! is a copy of the code, and module-system code also gets the
//! `.olean.server`, `.olean.private`, `.ir` and `.ir.sig` files Lean writes
//! for it. It reports the file's lines like fake-repl: `#check Nat` gives an
//! information message, `error_here` and an unmet `needs NAME` an error,
//! `sorry` a warning, `SLEEP <ms>` sleeps and `CRASH` exits. On errors, like
//! Lean, it exits 1 and writes nothing. Then it prints `PISA_IMPORTS`, the
//! modules the code imports, `PISA_DECLS`, its declarations (not for
//! module-system code), and `PISA_EXPORT`, their exports (see
//! `pisa_tests::declarations` and `pisa_tests::exports`). Both read the code
//! as `pisa_tests::as_reported` gives it, so `-- reported-as: LINE` makes them
//! show `LINE` in place of the next line, where the verifier sees the code.
//!
//! `pisa verify M1 … Mk` re-verifies snapshot modules, imports first: it
//! prints `PISA_IMPORTED {}`, then for each module, whose own code it reads
//! from its fake olean on LEAN_PATH, a `PISA_VERIFY` line with the module's
//! declarations (see `pisa_tests::declarations`). A `forge` marker in the code
//! stands in for the kernel catching a forged declaration: the module is
//! rejected and the run ends. A `SLOW_KERNEL <ms>` marker makes the module
//! take that long. `BREAK_KERNEL` makes the verifier exit 1 with an error on
//! stderr, and `FAIL_KERNEL` makes it report a failure of its own
//! (`PISA_FAILED`), as when it can't describe the module.

use std::io::Write;

use pisa_tests::{
    as_reported, declarations, declares, exports, find_module, imported_code, imports, is_module,
    needs, print_message, unknown_identifier,
};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => {
            let at = args.iter().position(|a| a == "-o").expect("-o");
            build(&args[at + 1], args.last().unwrap());
        }
        Some("verify") => verify(&args[1..]),
        _ => {
            eprintln!("usage: pisa build … | pisa verify MODULE…");
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
        if code.contains("BREAK_KERNEL") {
            eprintln!("uncaught exception: the fake kernel fell over");
            std::process::exit(1);
        }
        if code.contains("FAIL_KERNEL") {
            let failed =
                json!({"reason": format!("describing `{module}`: the fake describe failed")});
            writeln!(out, "PISA_FAILED {failed}").unwrap();
            out.flush().unwrap();
            std::process::exit(1);
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

fn build(olean: &str, file: &str) {
    let code = std::fs::read_to_string(file).unwrap();
    let imported = match imported_code(&code) {
        Ok(imported) => imported,
        Err(module) => {
            print_message(1, "error", &format!("unknown module prefix '{module}'"));
            std::process::exit(1);
        }
    };
    let mut failed = false;
    for (i, line) in code.lines().enumerate() {
        let n = i + 1;
        if let Some(ms) = line.split("SLEEP ").nth(1) {
            let ms: u64 = ms.trim().parse().unwrap_or(1000);
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
        if line.contains("CRASH") {
            eprintln!("fake-pisa: crashing as asked");
            std::process::exit(134);
        }
        if line.contains("#check Nat") {
            print_message(n, "information", "Nat : Type");
        }
        if line.contains("error_here") {
            failed = true;
            print_message(n, "error", "unexpected error_here");
        }
        if line.contains("sorry") {
            print_message(n, "warning", "declaration uses 'sorry'");
        }
    }
    for (n, name) in needs(&code) {
        if !declares(&imported, &code, name) {
            failed = true;
            let line = code.lines().nth(n - 1).unwrap_or_default();
            print_message(n, "error", &unknown_identifier(line, name));
        }
    }
    if failed {
        std::process::exit(1);
    }
    if is_module(&code) {
        for ext in ["olean.server", "olean.private", "ir", "ir.sig"] {
            let path = std::path::Path::new(olean).with_extension(ext);
            std::fs::write(path, ext).unwrap();
        }
    }
    std::fs::write(olean, &code).unwrap();
    println!("PISA_IMPORTS {}", json!(imports(&code)));
    let reported = as_reported(&code);
    if !is_module(&code) {
        println!("PISA_DECLS {}", declarations(&reported));
    }
    println!("PISA_EXPORT {}", exports(&reported));
}
