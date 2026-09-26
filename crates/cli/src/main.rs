//! The `haru` command.

fn main() -> std::process::ExitCode {
    haru_cli::main_with(haru_cli::Build::default())
}
