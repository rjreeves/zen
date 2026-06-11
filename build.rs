use chrono::{Datelike, Utc};

fn main() {
    println!("cargo:rerun-if-env-changed=ZEN_BUILD_NUMBER");

    let build_number = std::env::var("ZEN_BUILD_NUMBER").unwrap_or_else(|_| {
        let now = Utc::now();
        let rust_build_number = rust_build_number();

        format!(
            "{:02}{:02}{:02}.{}",
            now.year().rem_euclid(100),
            now.month(),
            now.day(),
            rust_build_number
        )
    });

    println!("cargo:rustc-env=ZEN_BUILD_NUMBER={}", build_number);
}

fn rust_build_number() -> String {
    std::env::var("CARGO_PKG_VERSION")
        .ok()
        .and_then(|version| {
            version
                .split_once('+')
                .map(|(version, _)| version)
                .unwrap_or(&version)
                .split_once('-')
                .map(|(version, _)| version)
                .unwrap_or(&version)
                .rsplit('.')
                .next()
                .map(str::to_string)
        })
        .filter(|part| !part.is_empty())
        .unwrap_or_else(|| "0".into())
}
