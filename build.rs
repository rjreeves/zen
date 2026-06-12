use chrono::{Datelike, Utc};
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=ZEN_BUILD_NUMBER");

    let build_number = std::env::var("ZEN_BUILD_NUMBER").unwrap_or_else(|_| {
        let now = Utc::now();
        let counter_path = build_counter_path();
        println!("cargo:rerun-if-changed={}", counter_path.display());
        let build_counter = next_build_counter(&counter_path);

        format!(
            "{:02}{:02}{:02}.{}",
            now.year().rem_euclid(100),
            now.month(),
            now.day(),
            build_counter
        )
    });

    println!("cargo:rustc-env=ZEN_BUILD_NUMBER={}", build_number);
}

fn build_counter_path() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    Path::new(&manifest_dir).join(".zen").join("build-number")
}

fn next_build_counter(path: &Path) -> u64 {
    let current = fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let next = current.saturating_add(1);

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, format!("{}\n", next));

    next
}
