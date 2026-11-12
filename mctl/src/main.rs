fn main() -> std::process::ExitCode {
    mctl::cli::run(std::env::args().skip(1).collect())
}
