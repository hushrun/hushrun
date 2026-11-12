//! The wrapper as its own binary, for tests and timing: cargo builds it with the tests, and a
//! link to it named `hushrun-rustc` is what `RUSTC_WRAPPER` points at.

fn main() -> std::process::ExitCode {
    hushrun_build::wrapper::main()
}
