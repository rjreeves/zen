mod cli;
mod lexer;
mod parser; // you'll add this next
mod ast;    // you'll add this next
mod audit;
mod permissions;
mod runtime;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "forge", version, about = "ForgeCLI v0.1")]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    Run {
        script: String,
        /// Automatically approve required permissions
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    Explain {
        script: String,
    },
    Audit,
    Version,
}

fn main() {
    env_logger::init();
    let args = Args::parse();

    if let Err(e) = cli::handle_command(args.command) {
        eprintln!("\n❌ {}\n", e);
        std::process::exit(1);
    }
}