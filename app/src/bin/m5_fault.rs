//! M5: tasks are unprivileged, and a fault kills only the task that caused it.
//! Three tasks fault three ways: a SysTick write (BusFault: core peripherals
//! are privileged-only), a stack overflow (UsageFault STKOF, via PSPLIM) and
//! an undefined instruction while holding a mutex (UsageFault UNDEFINSTR).
//! A privileged checker then verifies SysTick is untouched, time still runs,
//! an innocent task kept running, and the dead task's mutex stays locked.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering::SeqCst};
use cortex_m::peripheral::SYST;
use kernel::{Duration, Task};

/// Progress markers: 1 = about to fault, 2 = survived the fault (must not happen).
static ROGUE: AtomicU32 = AtomicU32::new(0);
static OVERFLOW: AtomicU32 = AtomicU32::new(0);
static HOLDER: AtomicU32 = AtomicU32::new(0);
static ALIVE: AtomicU32 = AtomicU32::new(0);
static RELOAD: AtomicU32 = AtomicU32::new(0);
kernel::mutex!(LOCK: () = ());

kernel::stack!(STACK_ALIVE, 512);
kernel::stack!(STACK_ROGUE, 512);
kernel::stack!(STACK_OVERFLOW, 256);
kernel::stack!(STACK_HOLDER, 512);
kernel::stack!(STACK_CHECKER, 1024);

fn alive() -> ! {
    loop {
        ALIVE.fetch_add(1, SeqCst);
        kernel::sleep(Duration::from_millis(1));
    }
}

fn rogue() -> ! {
    ROGUE.store(1, SeqCst);
    // SAFETY: deliberately illegal: SysTick belongs to the kernel.
    unsafe { (*SYST::PTR).rvr.write(1000) };
    ROGUE.store(2, SeqCst);
    loop {}
}

#[inline(never)]
fn recurse(n: u32) -> u32 {
    let buf = core::hint::black_box([n; 16]);
    if n == 0 {
        0
    } else {
        recurse(n - 1).wrapping_add(buf[3])
    }
}

fn overflow() -> ! {
    kernel::sleep(Duration::from_millis(2));
    OVERFLOW.store(1, SeqCst);
    core::hint::black_box(recurse(1000));
    OVERFLOW.store(2, SeqCst);
    loop {}
}

fn holder() -> ! {
    let _guard = LOCK.lock(None).unwrap();
    HOLDER.store(1, SeqCst);
    cortex_m::asm::udf();
}

fn checker() -> ! {
    kernel::sleep(Duration::from_millis(20));
    assert_eq!(
        ROGUE.load(SeqCst),
        1,
        "rogue task ran on after writing SysTick: not unprivileged, or not killed"
    );
    assert_eq!(OVERFLOW.load(SeqCst), 1, "overflowing task ran on");
    assert_eq!(HOLDER.load(SeqCst), 1, "faulting holder ran on");
    // SAFETY: this task is privileged; reading SysTick is allowed.
    let reload = unsafe { (*SYST::PTR).rvr.read() };
    assert_eq!(reload, RELOAD.load(SeqCst), "SysTick reload was changed");

    let (t0, a0) = (kernel::now(), ALIVE.load(SeqCst));
    kernel::sleep(Duration::from_millis(10));
    assert_eq!(
        kernel::now() - t0,
        Duration::from_millis(10),
        "time stopped"
    );
    let ran = ALIVE.load(SeqCst) - a0;
    assert!(ran >= 9, "innocent task ran only {} times in 10 ms", ran);

    assert!(
        LOCK.lock(Some(Duration::from_millis(5))).is_err(),
        "dead task's mutex was released"
    );
    defmt::info!(
        "3 tasks killed; SysTick intact; innocent task ran {} times in 10 ms",
        ran
    );
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    RELOAD.store(b.sys_hz / 1000 - 1, SeqCst);
    let mut tasks = [
        Task::new("alive", 1, alive, &STACK_ALIVE),
        Task::new("rogue", 2, rogue, &STACK_ROGUE),
        Task::new("overflow", 2, overflow, &STACK_OVERFLOW),
        Task::new("holder", 2, holder, &STACK_HOLDER),
        Task::new_privileged("checker", 3, checker, &STACK_CHECKER),
    ];
    kernel::start(&mut tasks, b.sys_hz)
}
