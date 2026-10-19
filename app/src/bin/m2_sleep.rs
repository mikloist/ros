//! M2: `sleep(ms)` lasts ms ticks (within one tick), `sleep_until` is
//! drift-free, a deadline in the past returns at once. While the only task
//! sleeps, the kernel's idle task must run (nothing else is ready).
#![no_std]
#![no_main]

use kernel::{Duration, Task};
use rp235x_hal as hal;

type Timer = hal::Timer<hal::timer::CopyableTimer0>;

/// Independent 1 MHz clock (TIMER0) to measure the kernel's SysTick-based sleeps.
static mut TIMER: Option<Timer> = None;
kernel::stack!(STACK, 1024);

fn micros() -> u64 {
    // SAFETY: written once in `main` before the kernel starts; read-only afterwards.
    unsafe { (*&raw const TIMER).unwrap() }
        .get_counter()
        .ticks()
}

fn sleeper() -> ! {
    for ms in [1u32, 2, 10, 100] {
        for _ in 0..3 {
            let t0 = micros();
            kernel::sleep(Duration::from_millis(ms as u64));
            let dt = micros() - t0;
            defmt::info!("sleep({}) took {} us", ms, dt);
            // Wakes on tick now+ms, and "now" is somewhere inside the current tick.
            let lo = (ms as u64 - 1) * 1000;
            let hi = ms as u64 * 1000 + 100; // 100 us for SysTick -> PendSV -> task
            assert!(
                dt >= lo && dt <= hi,
                "sleep({}) took {} us, want {}..={}",
                ms,
                dt,
                lo,
                hi
            );
        }
    }

    let start = kernel::now();
    for k in 1..=10 {
        kernel::sleep_until(start + Duration::from_millis(20 * k));
        assert_eq!(
            kernel::now(),
            start + Duration::from_millis(20 * k),
            "periodic wakeup drifted"
        );
    }
    defmt::info!("sleep_until: 10 periods of 20 ticks, no drift");

    let n = kernel::now();
    kernel::sleep_until(n - Duration::from_millis(5));
    assert!(
        kernel::now() - n <= Duration::from_millis(1),
        "sleep_until(past) blocked"
    );

    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    // SAFETY: no task is running yet.
    unsafe { *&raw mut TIMER = Some(b.timer) };
    // The sleeper reads TIMER0 itself: grant it that peripheral.
    let timer0 = hal::pac::TIMER0::PTR as usize;
    let mut tasks = [Task::new("sleeper", 1, sleeper, &STACK).with_peripheral(timer0, 0x100)];
    kernel::start(&mut tasks, b.sys_hz)
}
