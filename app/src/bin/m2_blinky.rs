//! M2 demo: the M1 blinky with `sleep` instead of polling. `led` is off the CPU
//! between toggles, so the kernel's idle task runs `wfi`: compare with
//! `m1_blinky`, which burned every cycle waiting.
#![no_std]
#![no_main]

use embedded_hal::digital::StatefulOutputPin;
use kernel::{Duration, Task};
use rp235x_hal as hal;

type Led =
    hal::gpio::Pin<hal::gpio::bank0::Gpio15, hal::gpio::FunctionSioOutput, hal::gpio::PullDown>;

/// Filled by `main` before `kernel::start`, taken once by `led`.
static mut LED: Option<Led> = None;
kernel::stack!(STACK_LED, 512);

fn led() -> ! {
    // SAFETY: written once in `main` before the kernel starts; only this task reads it.
    let mut pin = unsafe { (*&raw mut LED).take() }.unwrap();
    loop {
        kernel::sleep(Duration::from_millis(500));
        pin.toggle().unwrap();
        defmt::info!(
            "led {} at tick {}",
            if pin.is_set_high().unwrap() {
                "on "
            } else {
                "off"
            },
            kernel::now().ticks()
        );
    }
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    let pin = b.pins.gpio15.into_push_pull_output();
    // SAFETY: no task is running yet.
    unsafe { *&raw mut LED = Some(pin) };
    // led drives GP15 through SIO.
    let sio = hal::pac::SIO::PTR as usize;
    let mut tasks = [Task::new("led", 1, led, &STACK_LED).with_peripheral(sio, 0x200)];
    kernel::start(&mut tasks, b.sys_hz)
}
