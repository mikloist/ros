//! M6: the MPU confines each unprivileged task to flash, its own stack, the
//! shared RAM and the peripherals it was granted. Each `bad_*` task breaks one
//! rule and must die at that exact access; the others must not notice.
//!
//! | task          | does                                   | expect            |
//! |---------------|----------------------------------------|-------------------|
//! | `alive`       | counts in shared RAM, sleeps           | keeps running     |
//! | `granted`     | reads SIO (granted)                    | survives          |
//! | `bad_stack`   | reads `alive`'s stack                  | MemManage, killed |
//! | `bad_kernel`  | reads a semaphore in `.kobj`           | MemManage, killed |
//! | `bad_periph`  | reads SIO (not granted)                | MemManage, killed |
//! | `bad_exec`    | jumps into shared RAM (execute-never)  | MemManage, killed |
//! | `bad_object`  | gives a fake semaphore in shared RAM   | syscall refuses, killed |
//!
//! A privileged checker then reads the markers: 1 = reached the bad access,
//! 2 = ran past it (a hole in the protection).
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering::SeqCst};
use kernel::{Duration, Semaphore, Task};

static ALIVE: AtomicU32 = AtomicU32::new(0);
static GRANTED: AtomicU32 = AtomicU32::new(0);
static BAD_STACK: AtomicU32 = AtomicU32::new(0);
static BAD_KERNEL: AtomicU32 = AtomicU32::new(0);
static BAD_PERIPH: AtomicU32 = AtomicU32::new(0);
static BAD_EXEC: AtomicU32 = AtomicU32::new(0);
static BAD_OBJECT: AtomicU32 = AtomicU32::new(0);
/// `bx lr` (0x4770): valid code, but RAM is execute-never. `static mut` puts it
/// in RAM (`.data`); a plain `static` would land in flash, which may execute.
static mut RAM_CODE: [u16; 2] = [0x4770, 0x4770];
/// Lookalike of a semaphore in shared RAM, where any task could write it.
static mut FAKE_SEM: [u32; 8] = [0; 8];

kernel::semaphore!(REAL_SEM, 0);
kernel::stack!(STACK_ALIVE, 256);
kernel::stack!(STACK_GRANTED, 256);
kernel::stack!(STACK_BAD_STACK, 256);
kernel::stack!(STACK_BAD_KERNEL, 256);
kernel::stack!(STACK_BAD_PERIPH, 256);
kernel::stack!(STACK_BAD_EXEC, 256);
kernel::stack!(STACK_BAD_OBJECT, 256);
kernel::stack!(STACK_CHECKER, 1024);

/// SIO block (GPIO, CPUID). Its address is fixed in the RP2350 memory map.
const SIO: usize = 0xD000_0000;

fn park() -> ! {
    loop {
        kernel::sleep(Duration::from_secs(1));
    }
}

fn alive() -> ! {
    loop {
        ALIVE.fetch_add(1, SeqCst);
        kernel::sleep(Duration::from_millis(1));
    }
}

fn granted() -> ! {
    // SIO CPUID: 0 on core 0. Allowed: SIO is in this task's regions.
    let cpuid = unsafe { (SIO as *const u32).read_volatile() };
    GRANTED.store(if cpuid == 0 { 2 } else { 3 }, SeqCst);
    park()
}

fn bad_stack() -> ! {
    BAD_STACK.store(1, SeqCst);
    let other = STACK_ALIVE_ADDR.load(SeqCst) as *const u32;
    core::hint::black_box(unsafe { other.read_volatile() });
    BAD_STACK.store(2, SeqCst);
    park()
}

fn bad_kernel() -> ! {
    BAD_KERNEL.store(1, SeqCst);
    let sem = &REAL_SEM as *const Semaphore as *const u32;
    core::hint::black_box(unsafe { sem.read_volatile() });
    BAD_KERNEL.store(2, SeqCst);
    park()
}

fn bad_periph() -> ! {
    BAD_PERIPH.store(1, SeqCst);
    core::hint::black_box(unsafe { (SIO as *const u32).read_volatile() });
    BAD_PERIPH.store(2, SeqCst);
    park()
}

fn bad_exec() -> ! {
    BAD_EXEC.store(1, SeqCst);
    // Thumb: bit 0 set. Executing from an XN region faults on the fetch.
    let code = &raw const RAM_CODE as usize;
    let f: extern "C" fn() = unsafe { core::mem::transmute(code | 1) };
    f();
    BAD_EXEC.store(2, SeqCst);
    park()
}

fn bad_object() -> ! {
    BAD_OBJECT.store(1, SeqCst);
    // SAFETY: deliberately not: a forged object. The kernel must refuse it.
    let fake: &Semaphore = unsafe { &*(&raw const FAKE_SEM as *const Semaphore) };
    fake.give();
    BAD_OBJECT.store(2, SeqCst);
    park()
}

/// Address of `alive`'s stack, published by the privileged `main`.
static STACK_ALIVE_ADDR: AtomicU32 = AtomicU32::new(0);

fn checker() -> ! {
    kernel::sleep(Duration::from_millis(20));
    assert_eq!(GRANTED.load(SeqCst), 2, "granted task could not read its peripheral");
    for (name, m) in [
        ("bad_stack", &BAD_STACK),
        ("bad_kernel", &BAD_KERNEL),
        ("bad_periph", &BAD_PERIPH),
        ("bad_exec", &BAD_EXEC),
        ("bad_object", &BAD_OBJECT),
    ] {
        let v = m.load(SeqCst);
        assert!(v != 0, "{} never ran", name);
        assert!(v != 2, "{} ran past its bad access: not protected", name);
    }
    let a0 = ALIVE.load(SeqCst);
    kernel::sleep(Duration::from_millis(10));
    let ran = ALIVE.load(SeqCst) - a0;
    assert!(ran >= 9, "innocent task ran only {} times in 10 ms", ran);
    defmt::info!("5 tasks killed at their bad access; granted read ok; innocent task ran {} times", ran);
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    STACK_ALIVE_ADDR.store(&STACK_ALIVE as *const _ as u32, SeqCst);
    let mut tasks = [
        Task::new("alive", 1, alive, &STACK_ALIVE),
        Task::new("granted", 2, granted, &STACK_GRANTED).with_peripheral(SIO, 0x200),
        Task::new("bad_stack", 2, bad_stack, &STACK_BAD_STACK),
        Task::new("bad_kernel", 2, bad_kernel, &STACK_BAD_KERNEL),
        Task::new("bad_periph", 2, bad_periph, &STACK_BAD_PERIPH),
        Task::new("bad_exec", 2, bad_exec, &STACK_BAD_EXEC),
        Task::new("bad_object", 2, bad_object, &STACK_BAD_OBJECT),
        Task::new_privileged("checker", 3, checker, &STACK_CHECKER),
    ];
    kernel::start(&mut tasks, b.sys_hz)
}
