//! M0: proves a passing test binary makes `probe-rs run` exit 0.
#![no_std]
#![no_main]

#[app::entry]
fn main() -> ! {
    app::init();
    app::exit_ok()
}
