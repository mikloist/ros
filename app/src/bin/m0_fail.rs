//! M0: proves a failing assert makes `probe-rs run` exit non-zero.
#![no_std]
#![no_main]

#[app::entry]
fn main() -> ! {
    app::init();
    assert_eq!(1 + 1, 3, "deliberate failure");
    app::exit_ok()
}
