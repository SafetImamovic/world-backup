use std::process::ExitCode;

fn main() -> ExitCode {
    match world_backup::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
