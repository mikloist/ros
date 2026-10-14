//! M2: `sleep(ms)` lasts ms ticks (within one tick), `sleep_until` is
//! drift-free, a deadline in the past returns at once. While the only task
//! sleeps, the kernel's idle task must run (nothing else is ready).
#![no_std]
#![no_main]

use kernel::{Stack, Task};
use rp235x_hal as hal;

type Timer = hal::Timer<hal::timer::CopyableTimer0>;

/// Independent 1 MHz clock (TIMER0) to measure the kernel's SysTick-based sleeps.
static mut TIMER: Option<Timer> = None;
static STACK: Stack<1024> = Stack::new();

fn micros() -> u64 {
    // SAFETY: written once in `main` before the kernel starts; read-only afterwards.
    unsafe { (*&raw const TIMER).unwrap() }.get_counter().ticks()
}

fn sleeper() -> ! {
    for ms in [1u32, 2, 10, 100] {
        for _ in 0..3 {
            let t0 = micros();
            kernel::sleep(ms);
            let dt = micros() - t0;
            defmt::info!("sleep({}) took {} us", ms, dt);
            // Wakes on tick now+ms, and "now" is somewhere inside the current tick.
            let lo = (ms as u64 - 1) * 1000;
            let hi = ms as u64 * 1000 + 100; // 100 us for SysTick -> PendSV -> task
            assert!(dt >= lo && dt <= hi, "sleep({}) took {} us, want {}..={}", ms, dt, lo, hi);
        }
    }

    let start = kernel::now();
    for k in 1..=10 {
        kernel::sleep_until(start + 20 * k);
        assert_eq!(kernel::now(), start + 20 * k, "periodic wakeup drifted");
    }
    defmt::info!("sleep_until: 10 periods of 20 ticks, no drift");

    let n = kernel::now();
    kernel::sleep_until(n - 5);
    assert!(kernel::now() - n <= 1, "sleep_until(past) blocked");

    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    // SAFETY: no task is running yet.
    unsafe { *&raw mut TIMER = Some(b.timer) };
    let mut tasks = [Task::new("sleeper", 1, sleeper, &STACK)];
    kernel::start(&mut tasks, b.sys_hz)
}
