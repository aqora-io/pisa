//! A stand-in for `lake comparator` in tests, as pisa runs it: on the
//! challenge's and the solution's exports (`--challenge-from-export` and
//! `--solution-from-export`, as fake-leanexport writes them), building
//! nothing. Without both it can't start (exit 2), and so it can't with an
//! export of another module than the configuration names, or of other targets
//! than Lake would export, in Lake's order. Then it reads the configuration
//! and judges the solution by its code, printing its errors as comparator
//! does (`error: …` on stderr):
//! - a solution that says `sorry` is refused for its axiom, and one that says
//!   `SLEEP` takes 5 s;
//! - a definition name that isn't a `def` in both modules is refused, as
//!   comparator refuses one that isn't a definition;
//! - a module that says `-- CANNOT_RUN` keeps it from starting (exit 2);
//! - it runs the kernels `--paranoid` names, or those of the configuration's
//!   `external_kernels`, and then Lean's own: a solution that says
//!   `KERNEL_DECLINE` has con-leche and con-ron decline it (exit 2), and one
//!   that says `KERNEL_REJECT` has nanoda reject it, one that says
//!   `KERNEL_CANNOT_RUN` has lean4lean exit 3 with a line of its own, and one
//!   that says `KERNEL_KILLED` has con-ron exit 137, as a kernel the memory
//!   limit killed does, without a word.
//!
//! It prints its arguments first, for tests to read.

use std::process::exit;

use serde_json::Value;

fn fail(message: &str) -> ! {
    eprintln!("error: {message}");
    exit(1);
}

fn cannot_run(message: &str) -> ! {
    eprintln!("error: {message}");
    exit(2);
}

/// The constants the kernel builds in, which Lake exports from both sides
/// after the theorems and axioms, as Lake 4.36.0-rc2's `CLI/Check.lean`
/// lists them (`primitiveTargets`).
const PRIMITIVES: [&str; 26] = [
    "Nat.add",
    "Nat.sub",
    "Nat.mul",
    "Nat.pow",
    "Nat.gcd",
    "Nat.div",
    "Nat.mod",
    "Nat.beq",
    "Nat.ble",
    "Nat.land",
    "Nat.lor",
    "Nat.xor",
    "Nat.shiftLeft",
    "Nat.shiftRight",
    "String.ofList",
    "Char.ofNat",
    "List",
    "eagerReduce",
    "Nat",
    "String",
    "String.mk",
    "Char",
    "optParam",
    "autoParam",
    "semiOutParam",
    "outParam",
];

/// What Lake exports of each module for `config`: `Quot` and its parts if
/// `Quot.sound` is permitted, the theorems, the permitted axioms, the
/// kernel's own constants, and the definition holes.
fn targets(config: &Value) -> Vec<String> {
    let names = |key: &str| -> Vec<String> {
        let names = config[key].as_array().into_iter().flatten();
        names.filter_map(Value::as_str).map(String::from).collect()
    };
    let axioms = names("permitted_axioms");
    let mut targets = Vec::new();
    if axioms.iter().any(|a| a == "Quot.sound") {
        targets.extend(["Quot", "Quot.mk", "Quot.lift", "Quot.ind"].map(String::from));
    }
    targets.extend(names("theorem_names"));
    targets.extend(axioms);
    targets.extend(PRIMITIVES.map(String::from));
    targets.extend(names("definition_names"));
    targets
}

/// The code of the module exported to `path` as `config[key]` names it,
/// which must be what it exported, and of [`targets`].
fn exported(path: &str, config: &Value, key: &str) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        cannot_run(&format!("could not read the export {path}"));
    };
    let export: Value = serde_json::from_str(&text).unwrap();
    let module = config[key].as_str().unwrap_or_default();
    if export["module"] != module {
        cannot_run(&format!(
            "{path} exports {}, where the configuration's {key} is {module}",
            export["module"]
        ));
    }
    let wanted = targets(config);
    let given: Vec<String> = serde_json::from_value(export["targets"].clone()).unwrap();
    if given != wanted {
        cannot_run(&format!(
            "{path} exports {given:?}, where Lake would export {wanted:?}"
        ));
    }
    export["code"].as_str().unwrap_or_default().to_string()
}

/// Runs the kernels as comparator does, each printing its command, `Running`,
/// its own output and its verdict, Lean's default kernel last, and then fails
/// with the first failure: `kernels` are a configuration's `external_kernels`,
/// in Lake's order. nanoda rejects a solution that says `KERNEL_REJECT`, and
/// con-leche and con-ron decline one that says `KERNEL_DECLINE`, as they do a
/// non-standard axiom (exit 2).
fn kernels(kernels: &[(String, Vec<String>)], solution: &str) {
    let mut failure = None;
    let default = ("Lean default".to_string(), Vec::new());
    for (kernel, command) in kernels.iter().chain([&default]) {
        let kernel = kernel.as_str();
        // Lake prints no command: it is before `Running`, for tests to read,
        // so that the kernel's own first line is the first of its section.
        println!("{kernel} command: {}", command.join(" "));
        println!("Running {kernel} kernel on solution");
        let (verdict, exit) = match kernel {
            "nanoda" if solution.contains("KERNEL_REJECT") => ("rejected", Some(1)),
            "con-leche" | "con-ron" if solution.contains("KERNEL_DECLINE") => {
                eprintln!("{kernel}: declined: non-standard axiom (lit) (--verified)");
                ("rejected", Some(2))
            }
            "lean4lean" if solution.contains("KERNEL_CANNOT_RUN") => {
                eprintln!("lean4lean: cannot read the export (--import)");
                ("rejected", Some(3))
            }
            "con-ron" if solution.contains("KERNEL_KILLED") => ("rejected", Some(137)),
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
    let flag = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
    let (Some(challenge), Some(solution)) = (
        flag("--challenge-from-export"),
        flag("--solution-from-export"),
    ) else {
        cannot_run("the fake comparator judges exports only: it would build the modules");
    };
    let config = flag("--config").unwrap_or_else(|| "comparator.json".into());
    let Ok(text) = std::fs::read_to_string(&config) else {
        cannot_run(&format!("could not read the configuration {config}"));
    };
    let config: Value = serde_json::from_str(&text).unwrap();
    let challenge = exported(&challenge, &config, "challenge_module");
    let solution = exported(&solution, &config, "solution_module");
    if challenge.contains("-- CANNOT_RUN") || solution.contains("-- CANNOT_RUN") {
        cannot_run("the fake comparator can't run here");
    }
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
    if args.iter().any(|a| a == "--paranoid") {
        cannot_run("pisa names its kernels, and never passes --paranoid");
    }
    let named = config["external_kernels"].as_object().into_iter().flatten();
    let named: Vec<(String, Vec<String>)> = named
        .map(|(k, v)| (k.clone(), serde_json::from_value(v.clone()).unwrap()))
        .collect();
    kernels(&named, &solution);
    println!("Your solution is okay!");
}
