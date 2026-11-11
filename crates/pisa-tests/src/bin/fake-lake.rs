//! A stand-in for `lake comparator` in tests. It reads the configuration,
//! checks that the challenge's and the solution's files are there, and judges
//! the solution by its text, printing its steps and its errors as comparator
//! does (`Resolving dependencies`, `Building <module>`, then `error: …` on
//! stderr):
//! - a solution that says `sorry` is refused for its axiom, and one that says
//!   `SLEEP` takes 5 s;
//! - a definition name that isn't a `def` in both modules is refused, as
//!   comparator refuses one that isn't a definition;
//! - a module that says `BUILD_FAIL` fails to build, and one that says
//!   `CANNOT_RUN` keeps it from starting (exit 2);
//! - a module that says `SLOW_BUILD` takes 3 s to build, the first time.
//!
//! It prints its arguments first, for tests to read. It "builds" each module
//! once in the project: `Built <module>` the first time, with a mark under
//! `.lake/build`, and `Replayed <module>` after.

use std::path::Path;
use std::process::exit;

fn fail(message: &str) -> ! {
    eprintln!("error: {message}");
    exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("comparator") {
        eprintln!("fake-lake: only `comparator` is scripted");
        exit(2);
    }
    println!("fake-lake {}", args.join(" "));
    let config = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map_or("comparator.json", |w| w[1].as_str());
    let Ok(text) = std::fs::read_to_string(config) else {
        eprintln!("error: could not read the configuration {config}");
        exit(2);
    };
    let config: serde_json::Value = serde_json::from_str(&text).unwrap();
    println!("Resolving dependencies");
    let mut codes = Vec::new();
    for key in ["challenge_module", "solution_module"] {
        let module = config[key].as_str().unwrap_or_default();
        let path = format!("{}.lean", module.replace('.', "/"));
        let Ok(code) = std::fs::read_to_string(Path::new(&path)) else {
            fail(&format!("no module {module} at {path}"));
        };
        if code.contains("CANNOT_RUN") {
            eprintln!("error: the fake comparator can't run here");
            exit(2);
        }
        println!("Building {module}");
        if code.contains("BUILD_FAIL") {
            fail("Child exited with 1");
        }
        let built = Path::new(".lake/build").join(format!("{module}.built"));
        if code.contains("SLOW_BUILD") && !built.exists() {
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        if built.exists() {
            println!("Replayed {module}");
        } else {
            std::fs::create_dir_all(".lake/build").unwrap();
            std::fs::write(&built, "").unwrap();
            println!("Built {module}");
        }
        codes.push(code);
    }
    let (challenge, solution) = (&codes[0], &codes[1]);
    let definitions = config["definition_names"].as_array().into_iter().flatten();
    for name in definitions.filter_map(|n| n.as_str()) {
        let def = format!("def {name} ");
        if !challenge.contains(&def) {
            fail(&format!("Challenge constant is not a definition: '{name}'"));
        }
        if !solution.contains(&def) {
            fail(&format!("Solution constant is not a definition: '{name}'"));
        }
    }
    if solution.contains("SLEEP") {
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    if solution.contains("sorry") {
        fail("Illegal axiom detected: 'sorryAx'");
    }
    println!("Your solution is okay!");
}
