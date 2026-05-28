use chrono::{Datelike, TimeZone, Utc};

fn main() {
    println!("cargo:rerun-if-env-changed=ZEN_BUILD_NUMBER");

    let build_number = std::env::var("ZEN_BUILD_NUMBER").unwrap_or_else(|_| {
        let now = Utc::now();
        let month_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .single()
            .expect("valid UTC month start");
        let seconds = now.signed_duration_since(month_start).num_seconds();

        format!(
            "{:02}{:02}{:07}",
            now.year().rem_euclid(100),
            now.month(),
            seconds
        )
    });

    println!("cargo:rustc-env=ZEN_BUILD_NUMBER={}", build_number);
}
