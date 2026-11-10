//! A stand-in for `lake comparator` in tests. It reads the configuration,
//! checks that the challenge's and the solution's files are there, and judges
//! the solution by its text: one that says `sorry` is refused, and one that
//! says `SLEEP` takes 5 s. It prints its arguments first, for tests to read.

use std::path::Path;
use std::process::exit;

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
        eprintln!("fake-lake: no configuration {config}");
        exit(2);
    };
    let config: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut solution = String::new();
    for key in ["challenge_module", "solution_module"] {
        let module = config[key].as_str().unwrap_or_default();
        let path = format!("{}.lean", module.replace('.', "/"));
        match std::fs::read_to_string(Path::new(&path)) {
            Ok(code) => solution = code,
            Err(_) => {
                println!("error: no module {module} at {path}");
                exit(1);
            }
        }
    }
    if solution.contains("SLEEP") {
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    if solution.contains("sorry") {
        println!("error: Illegal axiom detected: 'sorryAx'");
        exit(1);
    }
    println!("Your solution is okay!");
}
