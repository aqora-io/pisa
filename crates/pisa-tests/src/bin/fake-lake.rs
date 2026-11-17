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
//! - it runs the kernels `--paranoid` names, or those of the configuration's
//!   `external_kernels`, and then Lean's own: a solution that says
//!   `KERNEL_DECLINE` has con-leche and con-ron decline it (exit 2), and one
//!   that says `KERNEL_REJECT` has nanoda reject it;
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

/// Runs the kernels as comparator does, each printing `Running`, its own
/// output and its verdict, Lean's default kernel last, and then fails with
/// the first failure: `names` are the kernels `--paranoid` runs, or a
/// configuration's `external_kernels`. nanoda rejects a solution that says
/// `KERNEL_REJECT`, and con-leche and con-ron decline one that says
/// `KERNEL_DECLINE`, as they do a non-standard axiom (exit 2).
fn kernels(names: &[String], solution: &str) {
    let mut failure = None;
    for kernel in names.iter().map(String::as_str).chain(["Lean default"]) {
        println!("Running {kernel} kernel on solution");
        let (verdict, exit) = match kernel {
            "nanoda" if solution.contains("KERNEL_REJECT") => ("rejected", Some(1)),
            "con-leche" | "con-ron" if solution.contains("KERNEL_DECLINE") => {
                eprintln!("{kernel}: declined: non-standard axiom (lit) (--verified)");
                ("rejected", Some(2))
            }
            _ => ("accepts", None),
        };
        println!("{kernel} kernel {verdict} the solution");
        if let Some(code) = exit {
            failure = failure.or(Some(format!("{kernel} exited with {code}")));
        }
    }
    if let Some(failure) = failure {
        fail(&failure);
    }
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
    let names: Vec<String> = if args.iter().any(|a| a == "--paranoid") {
        [
            "Lean paranoid",
            "lean4lean",
            "nanoda",
            "con-leche",
            "con-ron",
        ]
        .map(String::from)
        .to_vec()
    } else {
        let external = config["external_kernels"].as_object();
        external
            .into_iter()
            .flatten()
            .map(|(k, _)| k.clone())
            .collect()
    };
    println!(
        "Exporting #[t] from {}",
        config["solution_module"].as_str().unwrap_or_default()
    );
    kernels(&names, solution);
    println!("Your solution is okay!");
}
