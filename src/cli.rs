use std::fs;
use crate::ast::{Expr, PipeStage, Stmt};
use crate::lexer::Lexer;
use crate::audit::{write_entry, AuditEntry, current_timestamp};
use std::time::Instant;

use crate::Commands;
use crate::parser::Parser;
use crate::runtime::executor::Executor;
use crate::permissions::PermissionSet;
use std::io::{self, Write};
use serde_json;

pub fn handle_command(cmd: Commands) -> Result<(), String> {
    match cmd {
        Commands::Run { script, yes } => run_script(&script, yes),
        Commands::Explain { script } => explain_script(&script),
        Commands::Audit => show_audit(),
        Commands::Version => show_version(),
    }
}


fn run_script(path: &str, auto_yes: bool) -> Result<(), String> {
    let start = Instant::now();

    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read script '{}': {}", path, e))?;

    let tokens = Lexer::new(&src).tokenize()?;
    let mut parser = Parser::new(tokens, &src);
    let program = parser.parse_program()?;

    let permissions = PermissionSet::new(&program.requires);



    let permission_list = permissions.list();

    if !permission_list.is_empty() {
        println!("Script requires:");
        for p in &permission_list {
            println!("  - {}", p);
        }

        if !auto_yes {
            print!("\nAllow? (y/n): ");
            use std::io::{self, Write};
            io::stdout().flush().unwrap();

            let mut input = String::new();
            io::stdin().read_line(&mut input).unwrap();

            if input.trim().to_lowercase() != "y" {
                return Err("Execution denied by user".into());
            }
        } else {
            println!("✔ Auto-approved (--yes)");
        }
    }





    let permissions_list = permissions.list();

    let mut executor = Executor::new_with_permissions(permissions);

    let result = executor.execute(program);

    let duration = start.elapsed().as_millis();

    let audit_entry = AuditEntry {
        timestamp: current_timestamp(),
        script: path.to_string(),
        permissions: permissions_list,
        status: if result.is_ok() {
            "success".into()
        } else {
            "error".into()
        },
        duration_ms: duration,
        error: result.clone().err(),
    };

    let _ = write_entry(audit_entry);

    result
}



fn explain_script(path: &str) -> Result<(), String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read script '{}': {}", path, e))?;

    let tokens = Lexer::new(&src).tokenize()?;
    let mut parser = Parser::new(tokens, &src);
    let program = parser.parse_program()?;

    println!("Required permissions:");
    for (left, right) in program.requires {
        println!("  {}.{}", left, right);
    }

    println!("\nStatements:");
        for stmt in &program.statements {
        print_statement(stmt, 0);
        println!();
    }

    Ok(())
}


fn print_statement(stmt: &Stmt, indent: usize) {
    let pad = "  ".repeat(indent);

    match stmt {
        Stmt::Assignment { name, expr } => {
            println!("{}Assignment:", pad);
            println!("{}  name: {}", pad, name);
            println!("{}  value:", pad);
            print_expr(expr, indent + 2);
        }

        Stmt::Pipeline(p) => {
            println!("{}Pipeline:", pad);
            println!("{}  base:", pad);
            print_expr(&p.base, indent + 2);

            println!("{}  stages:", pad);
            for stage in &p.stages {
                print_stage(stage, indent + 2);
            }
        }

        Stmt::Expr(expr) => {
            println!("{}Expression:", pad);
            print_expr(expr, indent + 1);
        }
    }
}


fn print_stage(stage: &PipeStage, indent: usize) {
    let pad = "  ".repeat(indent);

    match stage {
        PipeStage::Where { expr } => {
            println!("{}Where:", pad);
            print_expr(expr, indent + 1);
        }

        PipeStage::Select { fields } => {
            println!("{}Select: {:?}", pad, fields);
        }

        PipeStage::Sort { field, descending } => {
            println!(
                "{}Sort: {} ({})",
                pad,
                field,
                if *descending { "desc" } else { "asc" }
            );
        }

        PipeStage::Limit { count } => {
            println!("{}Limit: {}", pad, count);
        }

        PipeStage::Count => {
            println!("{}Count", pad);
        }

        PipeStage::Sum { field } => {
            println!("{}Sum: {}", pad, field);
        }

        PipeStage::Avg { field } => {
            println!("{}Avg: {}", pad, field);
        }

        PipeStage::Max { field } => {
            println!("{}Max: {}", pad, field);
        }

        PipeStage::Min { field } => {
            println!("{}Min: {}", pad, field);
        }

        PipeStage::Distinct { field } => {
            println!("{}Distinct: {}", pad, field);
        }

        PipeStage::Call(call) => {
            println!("{}Call: {:?}", pad, call.name);
        }
    }
}


fn print_expr(expr: &Expr, indent: usize) {
    let pad = "  ".repeat(indent);

    match expr {
        Expr::Ident(name) => {
            println!("{}Ident({})", pad, name);
        }
        Expr::Literal(lit) => {
            println!("{}Literal({:?})", pad, lit);
        }
        Expr::Number(lit) => {
            println!("{}Number({:?})", pad, lit);
        }
        Expr::String(lit) => {
            println!("{}String({:?})", pad, lit);
        }
        Expr::Bool(lit) => {
            println!("{}Bool({:?})", pad, lit);
        }
        Expr::Unary { op, expr } => {
            println!("Unary: {:?}", op);
        }
   
        Expr::Call(call) => {
            println!("{}Call({:?})", pad, call.name);
        }

        Expr::Binary { left, op, right } => {
            println!("{}Binary {:?}", pad, op);
            print_expr(left, indent + 1);
            print_expr(right, indent + 1);
        }
    }
}


fn show_audit() -> Result<(), String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    let path = home.join(".zen").join("audit.log");

    if !path.exists() {
        println!("No audit log found.");
        return Ok(());
    }

    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read audit log: {}", e))?;

    let mut entries = Vec::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<AuditEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(_) => continue,
        }
    }

    if entries.is_empty() {
        println!("Audit log is empty.");
        return Ok(());
    }

    render_audit_table(&entries);

    Ok(())
}

fn render_audit_table(entries: &[AuditEntry]) {
    let headers = ["timestamp", "script", "status", "duration", "permissions"];

    // Compute column widths
    let mut w_timestamp = headers[0].len();
    let mut w_script = headers[1].len();
    let mut w_status = headers[2].len();
    let mut w_duration = headers[3].len();
    let mut w_perms = headers[4].len();

    for e in entries {
        w_timestamp = w_timestamp.max(e.timestamp.len());
        w_script = w_script.max(e.script.len());
        w_status = w_status.max(e.status.len());
        w_duration = w_duration.max(format!("{}ms", e.duration_ms).len());
        w_perms = w_perms.max(e.permissions.join(",").len());
    }

    // Header
    println!(
        "{:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}",
        headers[0],
        headers[1],
        headers[2],
        headers[3],
        headers[4],
        w1 = w_timestamp,
        w2 = w_script,
        w3 = w_status,
        w4 = w_duration,
        w5 = w_perms,
    );

    println!(
        "{:-<w1$}  {:-<w2$}  {:-<w3$}  {:-<w4$}  {:-<w5$}",
        "",
        "",
        "",
        "",
        "",
        w1 = w_timestamp,
        w2 = w_script,
        w3 = w_status,
        w4 = w_duration,
        w5 = w_perms,
    );

    // Rows
    for e in entries {
        println!(
            "{:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}",
            e.timestamp,
            e.script,
            e.status,
            format!("{}ms", e.duration_ms),
            e.permissions.join(","),
            w1 = w_timestamp,
            w2 = w_script,
            w3 = w_status,
            w4 = w_duration,
            w5 = w_perms,
        );
    }
}




fn show_version() -> Result<(), String> {
    println!("Zen v0.1.0");
    Ok(())
}