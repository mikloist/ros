//! M2 demo: the M1 blinky with `sleep` instead of polling. `led` sleeps 500 ms
//! between toggles; `ticker` logs once per second. Both are off the CPU almost
//! all the time, so the kernel's idle task runs `wfi` in between: compare
//! with `m1_blinky`, where both tasks burned every cycle.
#![no_std]
#![no_main]

use embedded_hal::digital::StatefulOutputPin;
use kernel::{Stack, Task};
use rp235x_hal as hal;

type Led = hal::gpio::Pin<hal::gpio::bank0::Gpio15, hal::gpio::FunctionSioOutput, hal::gpio::PullDown>;

/// Filled by `main` before `kernel::start`, taken once by `led`.
static mut LED: Option<Led> = None;
static STACK_LED: Stack<512> = Stack::new();
static STACK_TICKER: Stack<512> = Stack::new();

fn led() -> ! {
    // SAFETY: written once in `main` before the kernel starts; only this task reads it.
    let mut pin = unsafe { (*&raw mut LED).take() }.unwrap();
    loop {
        kernel::sleep(500);
        pin.toggle().unwrap();
    }
}

fn ticker() -> ! {
    let start = kernel::now();
    for s in 1.. {
        kernel::sleep_until(start + 1000 * s);
        defmt::info!("{} s (tick {})", s, kernel::now());
    }
    unreachable!()
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    let pin = b.pins.gpio15.into_push_pull_output();
    // SAFETY: no task is running yet.
    unsafe { *&raw mut LED = Some(pin) };
    let mut tasks = [Task::new("led", 2, led, &STACK_LED), Task::new("ticker", 1, ticker, &STACK_TICKER)];
    kernel::start(&mut tasks, b.sys_hz)
}
