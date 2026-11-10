//! A stand-in for `lean`, for tests: `lean --stdin --json` reports an import
//! conflict for each `import Clash` line on stdin, as Lean does.

use std::io::Read;

use pisa_tests::print_message;

fn main() {
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
