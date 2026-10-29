use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(llm_tags::worker::run() as u8)
}
