//! M2: preemption. Two priority-1 tasks spin forever and never yield; a
//! priority-3 task checks, in two phases:
//! 1. periodic sleeps: it wakes on the exact tick it asked for (preempts at
//!    once) and the spinners run while it sleeps;
//! 2. one long sleep: with no high-priority wakeups to reshuffle them, only
//!    SysTick time slicing can make both spinners progress (round robin).
//! A hang (exit 124) means the high task never got the CPU back: no preemption.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};
use kernel::{Duration, Task};

static SPIN_A: AtomicU32 = AtomicU32::new(0);
static SPIN_B: AtomicU32 = AtomicU32::new(0);
kernel::stack!(STACK_A, 256);
kernel::stack!(STACK_B, 256);
kernel::stack!(STACK_HIGH, 1024);

const PERIOD: Duration = Duration::from_millis(4);
const PERIODS: u32 = 50;
const SLICE_WINDOW: Duration = Duration::from_millis(100);

fn spin_a() -> ! {
    loop {
        SPIN_A.fetch_add(1, Relaxed);
    }
}

fn spin_b() -> ! {
    loop {
        SPIN_B.fetch_add(1, Relaxed);
    }
}

fn high() -> ! {
    let start = kernel::now();
    for k in 1..=PERIODS {
        let before = SPIN_A.load(Relaxed) + SPIN_B.load(Relaxed);
        kernel::sleep_until(start + PERIOD * k);
        assert_eq!(
            kernel::now(),
            start + PERIOD * k,
            "high task woke late: not preempting"
        );
        let after = SPIN_A.load(Relaxed) + SPIN_B.load(Relaxed);
        assert!(after > before, "spinners did not run while high slept");
    }
    defmt::info!("phase 1: {} on-time wakeups", PERIODS);

    // Phase 2: these stores race the spinners' increments; losing a few
    // counts is harmless next to millions per window.
    SPIN_A.store(0, Relaxed);
    SPIN_B.store(0, Relaxed);
    kernel::sleep(SLICE_WINDOW);
    let (a, b) = (SPIN_A.load(Relaxed), SPIN_B.load(Relaxed));
    defmt::info!(
        "phase 2: spin_a={} spin_b={} over {} ms",
        a,
        b,
        SLICE_WINDOW.as_millis() as u32
    );
    assert!(a > 0 && b > 0, "a spinner never ran: no time slicing");
    assert!(
        a.min(b) * 2 > a.max(b),
        "round robin unfair: {} vs {}",
        a,
        b
    );
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    let mut tasks = [
        Task::new("spin_a", 1, spin_a, &STACK_A),
        Task::new("spin_b", 1, spin_b, &STACK_B),
        Task::new("high", 3, high, &STACK_HIGH),
    ];
    kernel::start(&mut tasks, b.sys_hz)
}
