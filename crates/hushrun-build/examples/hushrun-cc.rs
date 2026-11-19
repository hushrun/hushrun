//! The C and C++ launcher as its own binary, for tests: cargo builds it with the tests, and the
//! tests run it through a link named `hushrun-cc`, as `HUSHRUN_CC` names it in a run.

fn main() -> std::process::ExitCode {
    hushrun_build::cc::main()
}
