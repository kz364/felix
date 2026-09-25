//! Check a rules file's `[[test]]` cases without the app.
//!
//! cargo run --example rules_check [rules.toml] [settings_store.json]
//!
//! Defaults to Handy's own files. Only vocabulary, corrections and
//! sound-alikes are read from the settings; nothing else is printed.

use std::path::PathBuf;

fn main() {
    let data = dirs_next();
    let mut args = std::env::args().skip(1);
    let rules = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("rules.toml"));
    let store = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("settings_store.json"));
    let results = match handy_app_lib::rules::check_file(&rules, Some(&store)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("✗ {}: {e}", rules.display());
            std::process::exit(2);
        }
    };
    let failed = results.iter().filter(|r| !r.passed).count();
    for r in &results {
        let mark = if r.passed { "✓" } else { "✗" };
        let model = if r.model_decides {
            "  (local model decides)"
        } else {
            ""
        };
        println!("{mark} {:?} → {:?}{model}", r.said, r.got);
        if !r.passed {
            println!("    expected {:?}", r.expect);
        }
    }
    println!("{} passed, {failed} failed", results.len() - failed);
    std::process::exit(if failed > 0 { 1 } else { 0 });
}

fn dirs_next() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Application Support/com.pais.handy")
}
