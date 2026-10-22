//! A stand-in for `lean`, for tests.
//!
//! `lean --stdin --json` reports an import conflict for each `import Clash`
//! line on stdin, as Lean does.
//!
//! `lean -R <root> -o <olean> --json <file>` compiles a module: the "olean" is
//! a copy of the code. It reports the file's lines like fake-repl: `#check Nat`
//! gives an information message, `error_here` and an unmet `needs NAME` an
//! error, `sorry` a warning, `SLEEP <ms>` sleeps and `CRASH` exits. On errors,
//! like Lean, it exits 1 and writes nothing.

use std::io::Read;

use pisa_tests::{declares, imported_code, needs};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().position(|a| a == "-o") {
        Some(i) => compile(&args[i + 1], args.last().unwrap()),
        None => diagnose(),
    }
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
    std::fs::write(olean, code).unwrap();
}
