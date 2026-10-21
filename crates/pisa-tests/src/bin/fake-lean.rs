//! A stand-in for `lean --stdin --json`, for tests: reports an import
//! conflict for each `import Clash` line on stdin, as Lean does.

use std::io::Read;

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let mut failed = false;
    for (i, line) in input.lines().enumerate() {
        if line.trim() == "import Clash" {
            failed = true;
            let message = serde_json::json!({
                "severity": "error",
                "pos": {"line": i + 1, "column": 0},
                "endPos": null,
                "fileName": "<stdin>",
                "data": "import Clash failed, environment already contains 'dup' from Other",
            });
            println!("{message}");
        }
    }
    std::process::exit(i32::from(failed));
}
