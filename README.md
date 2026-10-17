# small rust rtos for the pico 2 w

Small preemptive RTOS kernel for the Raspberry Pi Pico 2 W (RP2350), for me to understand how a kernel works: context switching, scheduling, time, synchronization, and later memory protection.

# AI usage
I use AI as a tutor and pair programmer. The split:

- Me: the design decisions (scope, Arm vs RISC-V, single core, static tasks, API and naming choices) and most of the kernel core (task frames, the scheduler, the ring buffer, wait lists), written by hand with AI explaining concepts and reviewing.
- AI: the per-milestone design spec and plans, the test harness (host tests and on-board test binaries), board plumbing and demos. Before each plan it builds a throwaway reference kernel and runs it on the board, so the tests and hints are known to be achievable; that reference is deleted, not copied in.

# Model
Single core (core 0), Arm Cortex-M33, no heap. Board bring-up is borrowed (`cortex-m-rt`, `rp235x-hal`); the kernel is written from scratch.

- Tasks are declared statically: a `Stack<N>` static plus `Task::new(name, priority, entry, &STACK)`, handed to `kernel::start(&mut tasks, sys_hz)`, which never returns.
- Context switch = PendSV (lowest exception priority). Hardware stacks half the registers, PendSV saves r4-r11, EXC_RETURN and, only if the task used the FPU, s16-s31.
- A new task's stack is pre-filled to look like it was interrupted just before its first instruction, so the first switch "returns" into it.
- Tasks run on PSP (Process Stack Pointer; the kernel and interrupt handlers use MSP, the Main Stack Pointer); PSPLIM (PSP limit register) is set per task, so a stack overflow faults in hardware instead of corrupting memory.
- All scheduler state lives in one `Sched` value (pure logic, host-tested). The only way in is `with_sched`, which runs inside a critical section (interrupts masked).
- Kernel lists are intrusive: the links live inside `Task` (`next` for ready/sleep lists, `wait_next` for wait lists), so a task can be on two lists at once with no allocation.

# Features
- Preemptive priority scheduler
    - 8 priorities, higher = more urgent; 0 is the kernel's idle task, apps use 1-7
    - Ready set = one FIFO per priority + a `u8` bitmap; highest ready priority via `leading_zeros` (CLZ)
    - Equal priorities round-robin every tick
- Time
    - SysTick at 1 kHz, `u64` tick count (never wraps)
    - `sleep(Duration)`, `sleep_until(Instant)` (drift-free periodic tasks), `now() -> Instant`, `yield_now()`
    - `Instant` = whole ticks since boot (`u64`, never wraps); durations are `core::time::Duration`, rounded up to whole ticks
    - Sleep list sorted by wake tick; SysTick only looks at the head
- Idle task runs `wfi` when nothing else is ready
- Semaphore and queue
    - Blocking calls take `Option<Duration>`: `None` waits forever, `Some(Duration::ZERO)` never blocks
    - Wait lists in priority order, FIFO on ties; the woken task gets the unit/item directly
    - A timed waiter is on the wait list and the sleep list at once; whichever fires first removes it from the other
    - `give`, `try_send`, `try_recv` are safe from interrupt handlers
- Planned: mutex with priority inheritance (M4), SVC syscalls + unprivileged tasks (M5), MPU isolation (M6), multicore (M7), then Wi-Fi via the CYW43 chip

# Layout
- `kernel/`: the RTOS (`no_std` lib). `task.rs`, `sched.rs`, `ring.rs` are host-testable; `arch/` and `sync.rs` are Cortex-M33 only
- `app/`: board init (`app::init()`), one test/demo binary per milestone in `app/src/bin/`
- `docs/superpowers/specs/`: design; `docs/superpowers/plans/`: one plan per milestone

# Testing
- Host: `cargo ktest` runs the kernel's pure-logic tests on the PC (task frames, scheduler, wait lists, ring buffer)
- Board: Pico 2 W behind a CMSIS-DAP probe, run with `cargo run --release --bin <name>` (`probe-rs` flashes it and streams `defmt` logs)
    - A binary passes with `TEST PASS` and exit 0 (semihosting exit); a failed `assert!` panics and exits 1
    - `m1_coop`: two tasks alternate via `yield_now`; FP registers survive switches
    - `m1_overflow`: overflowing a task stack faults (CFSR = `00100000`), exit 1 is the expected result
    - `m2_sleep`: sleeps last their tick count, periodic `sleep_until` has no drift
    - `m2_preempt`: a high-priority task preempts on time, equal priorities share the CPU
    - `m3_sem`, `m3_queue`: timeouts, counting, interrupt -> task wakeup (TIMER0 alarm), queue full/empty
- Demos: `m1_blinky` (cooperative, polls), `m2_blinky` (sleeps); external LED on GP15
