//! M4: mutual exclusion under preemption, then timeouts. Two equal-priority
//! tasks increment a shared counter with a read-modify-write that spans a tick
//! (so time slicing interrupts it mid-update); without the mutex, updates are
//! lost. Then a holder keeps the mutex while the checker tries to get it.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering::SeqCst};
use kernel::{Duration, Task};

const ROUNDS: u32 = 100;
const HOLD: Duration = Duration::from_millis(30);
kernel::mutex!(COUNTER: u32 = 0);
kernel::semaphore!(DONE, 0);
kernel::semaphore!(GO_HOLD, 0);
kernel::semaphore!(HOLDING, 0);
static FINISHED: AtomicU32 = AtomicU32::new(0);
kernel::stack!(STACK_A, 1024);
kernel::stack!(STACK_B, 1024);
kernel::stack!(STACK_HOLDER, 1024);
kernel::stack!(STACK_CHECKER, 1024);

fn incrementer() -> ! {
    for _ in 0..ROUNDS {
        let mut c = COUNTER.lock(None).unwrap();
        let v = *c;
        // Hold the lock across a tick boundary: the time slice ends here.
        let t = kernel::now();
        while kernel::now() == t {}
        *c = v + 1;
    }
    if FINISHED.fetch_add(1, SeqCst) == 1 {
        DONE.give();
    }
    loop {
        kernel::sleep(Duration::from_secs(1));
    }
}

fn holder() -> ! {
    GO_HOLD.take(None).unwrap();
    let guard = COUNTER.lock(None).unwrap();
    HOLDING.give();
    let t0 = kernel::now();
    while kernel::now() - t0 < HOLD {} // busy: must not depend on sleeping
    drop(guard);
    loop {
        kernel::sleep(Duration::from_secs(1));
    }
}

fn checker() -> ! {
    DONE.take(None).unwrap();
    {
        let c = COUNTER.lock(None).unwrap();
        defmt::info!("counter = {} (want {})", *c, 2 * ROUNDS);
        assert_eq!(*c, 2 * ROUNDS, "lost updates: mutex did not exclude");
    }

    GO_HOLD.give();
    HOLDING.take(None).unwrap();
    assert!(
        COUNTER.lock(Some(Duration::ZERO)).is_err(),
        "try-lock got a held mutex"
    );
    let t0 = kernel::now();
    assert!(
        COUNTER.lock(Some(Duration::from_millis(5))).is_err(),
        "timed lock got a held mutex"
    );
    assert_eq!(
        kernel::now() - t0,
        Duration::from_millis(5),
        "timed lock did not last 5 ms"
    );
    let got = COUNTER.lock(None);
    assert!(got.is_ok(), "untimed lock failed");
    assert!(
        kernel::now() - t0 < HOLD + Duration::from_millis(2),
        "holder kept the mutex too long"
    );
    defmt::info!("timeouts ok, lock handed over after the holder released it");
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    let mut tasks = [
        Task::new("a", 1, incrementer, &STACK_A),
        Task::new("b", 1, incrementer, &STACK_B),
        Task::new("holder", 1, holder, &STACK_HOLDER),
        Task::new("checker", 2, checker, &STACK_CHECKER),
    ];
    kernel::start(&mut tasks, b.sys_hz)
}
