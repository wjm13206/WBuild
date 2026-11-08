use std::process::ExitCode;

use clap::Parser;

use wbuild::cli::{self, Cli};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli::run(cli) {
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}
