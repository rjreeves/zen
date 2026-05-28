use std::io::{self, Write};

pub fn clear_screen() -> Result<(), String> {
    print!("\x1b[2J\x1b[3J\x1b[H");
    io::stdout()
        .flush()
        .map_err(|e| format!("Failed to clear terminal: {}", e))
}
