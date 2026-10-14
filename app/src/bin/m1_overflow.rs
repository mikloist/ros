//! M1: a task that overflows its stack must fault (PSPLIM), not corrupt memory.
//! Expected: `probe-rs run` exits 1 with a stack-overflow UsageFault.
//! If this prints `TEST PASS`, PSPLIM is not being set.
#![no_std]
#![no_main]

use kernel::{Stack, Task};

static STACK_A: Stack<256> = Stack::new();

#[inline(never)]
fn recurse(n: u32) -> u32 {
    let buf = core::hint::black_box([n; 16]);
    if n == 0 { 0 } else { recurse(n - 1).wrapping_add(buf[3]) }
}

fn task_a() -> ! {
    defmt::info!("recursing past 1 KiB stack");
    core::hint::black_box(recurse(1000));
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    app::init();
    let mut tasks = [Task::new("a", task_a, &STACK_A)];
    kernel::start(&mut tasks)
}
