mod audit;
mod cli;

use zen_lang::{ast, lexer, parser, runtime, terminal};

use zen_runtime::interrupt;
use zen_runtime::permissions;

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "zen", version, about = "Zen v9.0")]
struct Args {
    /// Override the workspace root used for state, plugins, workflows, and startup files
    #[arg(long, global = true, value_name = "PATH")]
    workspace: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    Run {
        script: String,
        /// Automatically approve required permissions
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    Repl {
        /// Automatically approve permissions declared during the session
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    Explain {
        script: String,
    },
    Audit,
    Runs,
    RunStatus {
        run_id: String,
    },
    Resume {
        run_id: String,
        /// Automatically approve required permissions
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    Version,
    #[command(external_subcommand)]
    OneOff(Vec<String>),
}

fn main() {
    env_logger::init();
    let args = Args::parse();

    if let Err(e) = cli::handle_command(
        args.command.unwrap_or(Commands::Repl { yes: false }),
        args.workspace,
    ) {
        eprintln!("\n❌ {}\n", e);
        std::process::exit(1);
    }
}
