use std::process::ExitCode;

use llm_tags::cli;
use llm_tags::env::Env;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    ExitCode::from(cli::run(argv, &Env::current()) as u8)
}
