//! M4: classic priority inversion. L (prio 1) holds a mutex for ~10 ms of CPU
//! work. H (prio 3) wakes at 2 ms and blocks on it. M (prio 2) wakes at 3 ms
//! and burns the CPU for 200 ms. Without inheritance M starves L, so H waits
//! ~200 ms; with it L runs at H's priority, finishes, and H gets the mutex at
//! ~10 ms, before M is done.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering::SeqCst};
use kernel::{Duration, Instant, Task};

kernel::mutex!(LOCK: () = ());
/// Bit 0: H got the mutex. Bit 1: M finished burning.
static EVENTS: AtomicU32 = AtomicU32::new(0);
kernel::stack!(STACK_L, 512);
kernel::stack!(STACK_M, 512);
kernel::stack!(STACK_H, 1024);

/// Busy CPU work until `until` (preemptible, unlike sleep).
fn burn_until(until: Instant) {
    while kernel::now() < until {}
}

fn low() -> ! {
    let start = kernel::now();
    let guard = LOCK.lock(None).unwrap();
    burn_until(start + Duration::from_millis(10));
    drop(guard);
    loop {
        kernel::sleep(Duration::from_secs(1));
    }
}

fn medium() -> ! {
    kernel::sleep(Duration::from_millis(3));
    let t0 = kernel::now();
    burn_until(t0 + Duration::from_millis(200));
    EVENTS.fetch_or(2, SeqCst);
    loop {
        kernel::sleep(Duration::from_secs(1));
    }
}

fn high() -> ! {
    kernel::sleep(Duration::from_millis(2));
    let t0 = kernel::now();
    let guard = LOCK.lock(None).unwrap();
    let waited = kernel::now() - t0;
    EVENTS.fetch_or(1, SeqCst);
    drop(guard);
    defmt::info!("H waited {} ms for the mutex", waited.as_millis() as u32);
    assert_eq!(
        EVENTS.load(SeqCst) & 2,
        0,
        "M finished first: priority inversion"
    );
    assert!(
        waited < Duration::from_millis(15),
        "H waited {} ms",
        waited.as_millis() as u32
    );
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    let mut tasks = [
        Task::new("low", 1, low, &STACK_L),
        Task::new("medium", 2, medium, &STACK_M),
        Task::new("high", 3, high, &STACK_H),
    ];
    kernel::start(&mut tasks, b.sys_hz)
}
