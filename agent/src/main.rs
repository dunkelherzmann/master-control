fn main() -> std::process::ExitCode {
    mc_agent::cli::run(std::env::args().skip(1).collect())
}
