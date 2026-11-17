//! A stand-in for `lean`, for tests: `lean --stdin --json` reports an import
//! conflict for each `import Clash` line on stdin, as Lean does, and
//! `lean --print-prefix` names a toolchain that isn't there.

use std::io::Read;

use pisa_tests::print_message;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--print-prefix") {
        println!("/fake-toolchain");
        return;
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let mut failed = false;
    for (i, line) in input.lines().enumerate() {
        if line.trim() == "import Clash" {
            failed = true;
            print_message(
                i + 1,
                "error",
                "import Clash failed, environment already contains 'dup' from Other",
            );
        }
    }
    std::process::exit(i32::from(failed));
}
