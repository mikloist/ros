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
- Tasks run unprivileged and reach the kernel only through `svc`: the `SVCall` handler reads the syscall number from the instruction and the arguments from the saved registers, and writes the result back into the saved r0. A call that blocks gets its result written when the task is switched back in.
- Kernel lists are intrusive: the links live inside `Task` (`next` for ready/sleep lists, `wait_next` for wait lists), so a task can be on two lists at once with no allocation. One generic type, `TaskList<L>` (`list.rs`), covers both; the link field is part of the type (`TaskList<Next>`, `WaitList = TaskList<WaitNext>`).

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
- Mutex with priority inheritance
    - `lock(timeout)` returns a guard that derefs to the data and unlocks on drop; not recursive, task context only
    - The owner runs at the priority of its most urgent waiter, through chains of mutexes; recomputed on block, unlock and timeout
    - Unlock hands the mutex directly to the most urgent waiter
- Syscalls and protection
    - Tasks are unprivileged; touching core peripherals (SysTick, NVIC, SCB) faults
    - A fault in a task (bad access, stack overflow, undefined instruction) kills only that task; a fault in the kernel or an interrupt handler halts
    - Critical sections work for unprivileged tasks too (the kernel raises BASEPRI for them), so `defmt` logging is safe from any task
- Planned: MPU isolation (M6), multicore (M7), then Wi-Fi via the CYW43 chip

# Layout
- `kernel/`: the RTOS (`no_std` lib). `task.rs`, `list.rs`, `sched.rs`, `ring.rs` are host-testable; `arch/` and `sync.rs` are Cortex-M33 only
- `app/`: board init (`app::init()`), one test/demo binary per milestone in `app/src/bin/`
- `docs/superpowers/specs/`: design; `docs/superpowers/plans/`: one plan per milestone

# Testing
- Host: `cargo ktest` runs the kernel's pure-logic tests on the PC (task frames, scheduler, wait lists, ring buffer)
- Board: Pico 2 W behind a CMSIS-DAP probe, run with `cargo run --release --bin <name>` (`probe-rs` flashes it and streams `defmt` logs)
    - A binary passes with `TEST PASS` and exit 0 (semihosting exit); a failed `assert!` panics and exits 1
    - `m1_coop`: two tasks alternate via `yield_now`; FP registers survive switches
    - `m2_sleep`: sleeps last their tick count, periodic `sleep_until` has no drift
    - `m2_preempt`: a high-priority task preempts on time, equal priorities share the CPU
    - `m3_sem`, `m3_queue`: timeouts, counting, interrupt -> task wakeup (TIMER0 alarm), queue full/empty
    - `m4_inversion`, `m4_mutex`: priority inversion resolved, mutual exclusion under time slicing, mutex timeouts
    - `m5_fault`: three tasks fault three ways (SysTick write, stack overflow, undefined instruction) and only they die; time, SysTick and an innocent task are unaffected
- Demos: `m1_blinky` (cooperative, polls), `m2_blinky` (sleeps); external LED on GP15
