//! M7: both cores run tasks from one global ready set.
//!
//! 1. Speed-up: two CPU-bound tasks finish together in about the time one
//!    takes alone (one core would need twice as long).
//! 2. Cross-core wake-up: a prio-4 task on one core gives a semaphore; the
//!    prio-3 waiter must preempt the prio-1 spinner on the *other* core at
//!    once (doorbell), not at the next tick (up to 1 ms later).
//! 3. Ping-pong stress: two tasks hand a token back and forth through two
//!    semaphores, so a task is woken by the other core just as its own core
//!    switches it out: the race that `Task::on_cpu` guards against.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering::SeqCst};
use kernel::{Duration, Task};

/// TIMER0 TIMERAWL: free-running microseconds (tasks below are granted TIMER0).
const TIMER0: usize = 0x400B_0000;
fn micros() -> u32 {
    unsafe { ((TIMER0 + 0x28) as *const u32).read_volatile() }
}

const WORK: u32 = 1_500_000;
const WAKE_ROUNDS: u32 = 20;
const PING_ROUNDS: u32 = 20_000;

kernel::semaphore!(GO, 0);
kernel::semaphore!(DONE, 0);
kernel::semaphore!(WAKE, 0);
kernel::semaphore!(WAKE_GO, 0);
kernel::semaphore!(WAKE_DONE, 0);
kernel::semaphore!(PING, 0);
kernel::semaphore!(PONG, 0);
kernel::semaphore!(PING_GO, 0);
kernel::semaphore!(PING_DONE, 0);
kernel::mutex!(COUNTER: u32 = 0);

static GIVEN_AT: AtomicU32 = AtomicU32::new(0);
static WORST_WAKE: AtomicU32 = AtomicU32::new(0);

kernel::stack!(STACK_W0, 256);
kernel::stack!(STACK_W1, 256);
kernel::stack!(STACK_SPIN, 256);
kernel::stack!(STACK_GIVER, 512);
kernel::stack!(STACK_WAITER, 512);
kernel::stack!(STACK_PING, 512);
kernel::stack!(STACK_PONG, 512);
kernel::stack!(STACK_CHECKER, 1024);

fn worker() -> ! {
    loop {
        GO.take(None).unwrap();
        let mut x = 0u32;
        for i in 0..WORK {
            x = core::hint::black_box(x.wrapping_add(i));
        }
        DONE.give();
    }
}

fn spinner() -> ! {
    WAKE_GO.take(None).unwrap(); // phase 2 only: it would slow phase 1
    WAKE_GO.give(); // pass the start signal on to the giver
    loop {
        core::hint::black_box(0);
    }
}

fn giver() -> ! {
    WAKE_GO.take(None).unwrap();
    for _ in 0..WAKE_ROUNDS {
        kernel::sleep(Duration::from_millis(3));
        GIVEN_AT.store(micros(), SeqCst);
        WAKE.give();
        // Keep this core busy: the waiter has to get the *other* core.
        let t0 = micros();
        while micros().wrapping_sub(t0) < 2_000 {}
    }
    WAKE_DONE.give();
    loop {
        kernel::sleep(Duration::from_secs(1));
    }
}

fn waiter() -> ! {
    loop {
        WAKE.take(None).unwrap();
        let dt = micros().wrapping_sub(GIVEN_AT.load(SeqCst));
        WORST_WAKE.fetch_max(dt, SeqCst);
    }
}

fn ping() -> ! {
    PING_GO.take(None).unwrap();
    for _ in 0..PING_ROUNDS {
        *COUNTER.lock(None).unwrap() += 1;
        PONG.give();
        PING.take(None).unwrap();
    }
    PING_DONE.give();
    loop {
        kernel::sleep(Duration::from_secs(1));
    }
}

fn pong() -> ! {
    for _ in 0..PING_ROUNDS {
        PONG.take(None).unwrap();
        *COUNTER.lock(None).unwrap() += 1;
        PING.give();
    }
    loop {
        kernel::sleep(Duration::from_secs(1));
    }
}

fn checker() -> ! {
    // 1. Speed-up.
    let t0 = kernel::now();
    GO.give();
    DONE.take(None).unwrap();
    let one = kernel::now() - t0;
    let t0 = kernel::now();
    GO.give();
    GO.give();
    DONE.take(None).unwrap();
    DONE.take(None).unwrap();
    let two = kernel::now() - t0;
    defmt::info!("one task {} ms, two tasks {} ms", one.as_millis() as u32, two.as_millis() as u32);
    assert!(one >= Duration::from_millis(10), "work too short to measure");
    assert!(two * 2 < one * 3, "two tasks took {} ms vs {} ms alone: not running in parallel",
        two.as_millis() as u32, one.as_millis() as u32);

    // 2. Cross-core wake-up latency.
    WAKE_GO.give();
    WAKE_DONE.take(None).unwrap();
    let worst = WORST_WAKE.load(SeqCst);
    defmt::info!("cross-core wake-up: worst {} us over {} rounds", worst, WAKE_ROUNDS);
    assert!(worst < 100, "waiter woke {} us late: the other core was not told", worst);

    // 3. Ping-pong stress.
    let t0 = kernel::now();
    PING_GO.give();
    PING_DONE.take(Some(Duration::from_secs(10))).expect("ping-pong stalled");
    let count = *COUNTER.lock(None).unwrap();
    defmt::info!("ping-pong: {} handoffs in {} ms", count, (kernel::now() - t0).as_millis() as u32);
    assert_eq!(count, 2 * PING_ROUNDS, "lost updates");
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let mut b = app::init();
    b.start_core1();
    let mut tasks = [
        Task::new("w0", 1, worker, &STACK_W0),
        Task::new("w1", 1, worker, &STACK_W1),
        Task::new("spin", 1, spinner, &STACK_SPIN),
        Task::new("giver", 4, giver, &STACK_GIVER).with_peripheral(TIMER0, 0x100),
        Task::new("waiter", 3, waiter, &STACK_WAITER).with_peripheral(TIMER0, 0x100),
        Task::new("ping", 2, ping, &STACK_PING),
        Task::new("pong", 2, pong, &STACK_PONG),
        Task::new("checker", 5, checker, &STACK_CHECKER),
    ];
    kernel::start(&mut tasks, b.sys_hz)
}
