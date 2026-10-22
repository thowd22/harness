//! Standalone sandbox launcher: applies Landlock write rules, then runs the
//! command. See `harness_policy::sandbox`.

fn main() {
    std::process::exit(harness_policy::sandbox::launcher_main(
        std::env::args_os().skip(1),
    ));
}
