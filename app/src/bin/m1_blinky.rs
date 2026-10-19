//! M1 demo: cooperative blinky. `led` toggles GP15 every 500 ms by polling the
//! timer and yielding while it waits; `counter` just counts its turns. The log
//! shows how often `counter` ran per blink: with no preemption and no sleep,
//! both tasks stay busy the whole time (M2's `sleep` fixes that).
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};
use embedded_hal::digital::StatefulOutputPin;
use kernel::Task;
use rp235x_hal as hal;

type Led =
    hal::gpio::Pin<hal::gpio::bank0::Gpio15, hal::gpio::FunctionSioOutput, hal::gpio::PullDown>;
type Timer = hal::Timer<hal::timer::CopyableTimer0>;

/// Filled by `main` before `kernel::start`, taken once by `led`.
static mut LED: Option<(Led, Timer)> = None;
static TURNS: AtomicU32 = AtomicU32::new(0);
kernel::stack!(STACK_LED, 512);
kernel::stack!(STACK_COUNTER, 512);

const HALF_PERIOD_US: u64 = 500_000;

fn led() -> ! {
    // SAFETY: written once in `main` before the kernel starts; only this task reads it.
    let (mut pin, timer) = unsafe { (*&raw mut LED).take() }.unwrap();
    let mut next = timer.get_counter().ticks();
    loop {
        next += HALF_PERIOD_US;
        while timer.get_counter().ticks() < next {
            kernel::yield_now(); // cooperative "wait": let counter run
        }
        pin.toggle().unwrap();
        let on = pin.is_set_high().unwrap();
        defmt::info!(
            "led {} | counter ran {} times",
            if on { "on " } else { "off" },
            TURNS.swap(0, Relaxed)
        );
    }
}

fn counter() -> ! {
    loop {
        TURNS.fetch_add(1, Relaxed);
        kernel::yield_now();
    }
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    let pin = b.pins.gpio15.into_push_pull_output();
    // SAFETY: no task is running yet.
    unsafe { *&raw mut LED = Some((pin, b.timer)) };
    let mut tasks = [
        // led drives GP15 (SIO) and reads TIMER0.
        Task::new("led", 1, led, &STACK_LED)
            .with_peripheral(hal::pac::SIO::PTR as usize, 0x200)
            .with_peripheral(hal::pac::TIMER0::PTR as usize, 0x100),
        Task::new("counter", 1, counter, &STACK_COUNTER),
    ];
    kernel::start(&mut tasks, b.sys_hz)
}
