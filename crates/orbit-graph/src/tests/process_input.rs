//! The library reads no environment variable or other process input: the
//! composition layer resolves those once and installs them as a
//! [`crate::RuntimeConfig`] (`STD-02 §R3`).

use std::fs;
use std::path::{Path, PathBuf};

/// Calls that read process input.
const PROCESS_INPUT: &[&str] = &[
    "env::var",
    "env::vars",
    "var_os(",
    "env::args",
    "args_os(",
    "env::temp_dir",
    "env::set_var",
    "env::remove_var",
];

#[test]
fn production_sources_read_no_environment_variables() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    for file in production_sources(&root) {
        let text = fs::read_to_string(&file).expect("read source");
        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            if PROCESS_INPUT.iter().any(|call| code.contains(call)) {
                offenders.push(format!(
                    "{}:{}: {}",
                    file.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the library read process input; resolve it in the CLI and pass it down:\n{}",
        offenders.join("\n")
    );
}

/// Every `.rs` file under `dir` outside a `tests` directory.
fn production_sources(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).expect("read source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "tests") {
                files.extend(production_sources(&path));
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}
