use serde::{Serialize, Deserialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use dirs;
use chrono::Utc;

#[derive(Serialize, Deserialize, Debug)]
pub struct AuditEntry {
    pub timestamp: String,
    pub script: String,
    pub permissions: Vec<String>,
    pub status: String,
    pub duration_ms: u128,
    pub error: Option<String>,
}

pub fn write_entry(entry: AuditEntry) -> Result<(), String> {
    let mut path = get_audit_path()?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;

    let json = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
    writeln!(file, "{}", json).map_err(|e| e.to_string())?;

    Ok(())
}

fn get_audit_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(".zen").join("audit.log"))
}

pub fn current_timestamp() -> String {
   // Utc::now().to_rfc3339()
   Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}