//! A stand-in for `lean`, for tests.
//!
//! `lean --stdin --json` reports an import conflict for each `import Clash`
//! line on stdin, as Lean does.
//!
//! `lean --run <verifier> <module>` re-verifies a snapshot: it loads the
//! module's own code from its fake olean and prints a `PISA_VERIFY` line with
//! its declarations, reporting `ok:false` when the code contains a `forge`
//! marker (standing in for the kernel catching a forged declaration). A
//! `SLOW_KERNEL <ms>` marker in the code makes it take that long.
//!
//! `lean -R <root> -o <olean> --json <file>` compiles a module: the "olean" is
//! a copy of the code, and module-system code also gets the `.olean.server`,
//! `.olean.private`, `.ir` and `.ir.sig` files Lean writes for it. It reports
//! the file's lines like fake-repl: `#check Nat` gives an information
//! message, `error_here` and an unmet `needs NAME` an error, `sorry` a
//! warning, `SLEEP <ms>` sleeps and `CRASH` exits. On errors, like Lean, it
//! exits 1 and writes nothing.

use std::io::Read;

use pisa_tests::{declarations, declares, find_module, imported_code, is_module, needs};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--run") {
        // `lean --run <verifier> <module>`: re-verify a snapshot module.
        verify(args.last().unwrap());
    } else if let Some(i) = args.iter().position(|a| a == "-o") {
        compile(&args[i + 1], args.last().unwrap());
    } else {
        diagnose();
    }
}

/// The snapshot verifier's stand-in: load the module's own code from its fake
/// olean on LEAN_PATH, report its declarations, and reject it when the code
/// carries a `forge` marker (standing in for the kernel catching a forgery).
fn verify(module: &str) {
    let code = find_module(module)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .unwrap_or_default();
    if let Some(ms) = code.split("SLOW_KERNEL ").nth(1) {
        let ms: u64 = ms.lines().next().unwrap().trim().parse().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    let verdict = if code.contains("forge") {
        json!({"ok": false, "reason": "kernel re-check: forged declaration"})
    } else {
        let decls: serde_json::Value = serde_json::from_str(&declarations(&code)).unwrap();
        json!({"ok": true, "declarations": decls})
    };
    println!("PISA_VERIFY {verdict}");
}

fn report(line: usize, severity: &str, data: &str) {
    let message = json!({
        "severity": severity,
        "pos": {"line": line, "column": 0},
        "endPos": null,
        "fileName": "<stdin>",
        "data": data,
    });
    println!("{message}");
}

fn diagnose() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let mut failed = false;
    for (i, line) in input.lines().enumerate() {
        if line.trim() == "import Clash" {
            failed = true;
            report(
                i + 1,
                "error",
                "import Clash failed, environment already contains 'dup' from Other",
            );
        }
    }
    std::process::exit(i32::from(failed));
}

fn compile(olean: &str, file: &str) {
    let code = std::fs::read_to_string(file).unwrap();
    let imported = match imported_code(&code) {
        Ok(imported) => imported,
        Err(module) => {
            report(1, "error", &format!("unknown module prefix '{module}'"));
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
            eprintln!("fake-lean: crashing as asked");
            std::process::exit(134);
        }
        if line.contains("#check Nat") {
            report(n, "information", "Nat : Type");
        }
        if line.contains("error_here") {
            failed = true;
            report(n, "error", "unexpected error_here");
        }
        if line.contains("sorry") {
            report(n, "warning", "declaration uses 'sorry'");
        }
    }
    for (n, name) in needs(&code) {
        if !declares(&imported, &code, name) {
            failed = true;
            report(n, "error", &format!("Unknown identifier `{name}`"));
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
    std::fs::write(olean, code).unwrap();
}
