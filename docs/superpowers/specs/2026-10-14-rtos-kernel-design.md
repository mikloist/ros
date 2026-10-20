# ros: RTOS kernel for Raspberry Pi Pico 2 W — design

Learning project: write a preemptive RTOS kernel in Rust for the RP2350 (Pico 2 W).

## Decisions

| Topic | Choice |
|---|---|
| Depth | Kernel from scratch; borrow platform (`cortex-m-rt`, `rp235x-hal`). Embassy removed. |
| Cores | Arm Cortex-M33 (`thumbv8m.main-none-eabihf`), Secure state, no TrustZone setup. M8 adds the RP2350's Hazard3 RISC-V cores (`riscv32imac-unknown-none-elf`) as a second build target. |
| SMP | Core 0 only through M6. Critical sections behind one `arch` API so M7 (multicore) can swap them. |
| v1 scope | Levels 1–4: context switch, preemptive priority scheduler, semaphore/mutex/queue, SVC + unprivileged tasks + MPU. |
| Structure | FreeRTOS-style, static task table, shared address space, no heap. Scheduler handles `*mut Task` only, so runtime task creation (heap-backed) can be added later without rewriting it. |
| Debug I/O | `defmt` over RTT, `probe-rs run`. External LED on GP15 (onboard LED needs CYW43). |

Out of scope for v1 (each gets its own spec): runtime task creation, second core (M7, after M6), Hazard3 RISC-V port (M8, after M7), CYW43 Wi-Fi (after M8, Arm only), tickless idle.

## 1. Layout and boot

```
ros/
├─ Cargo.toml          workspace: kernel, app
├─ kernel/             lib, no_std; host-testable except arch/
│  └─ src/
│     ├─ lib.rs        public API: start(), sleep(), sleep_until(), yield_now(), now(), sync types
│     ├─ arch/
│     │  ├─ mod.rs     start, PendSV/kernel_switch, SysTick, with_sched, public time API
│     │  ├─ svc.rs     Sys/Status, svc(), SVCall + svc_dispatch, from_handler, critical-section impl
│     │  ├─ fault.rs   MemManage/BusFault/UsageFault: kill the task or halt
│     │  └─ mpu.rs     (M6)
│     ├─ task.rs
│     ├─ sched.rs
│     ├─ collections/  allocation-free containers, host-tested
│     │  ├─ list.rs    TaskList<L>: intrusive task lists (next / wait_next)
│     │  └─ ring.rs    Ring<T, N>: queue storage
│     └─ sync/         each op = kernel half (returns Status) + task half (makes the syscall)
│        ├─ mod.rs     Timeout, re-exports
│        ├─ semaphore.rs
│        ├─ queue.rs
│        └─ mutex.rs
└─ app/                bin; board init, task table, demos
   ├─ src/main.rs
   ├─ src/bin/         one demo per milestone
   ├─ memory.x, build.rs, .cargo/config.toml   (moved from root)
   └─ cyw43-firmware/  (kept for later Wi-Fi spec)
```

- Boot: `cortex-m-rt` reset/vector table; `rp235x-hal` provides `IMAGE_DEF` block, clocks (150 MHz), GPIO, UART.
- Only `arch/` touches registers. Its interface to the rest of the kernel: start first task, pend a switch, start tick, enter/exit critical section.
- `sched`, `time`, `sync` are pure logic and run under `cargo test` on the host.

## 2. Tasks and context switch

```rust
pub struct Task {
    sp: *mut u32,              // saved PSP
    stack_limit: *mut u32,     // PSPLIM
    state: State,              // Ready | Running | Sleeping | Blocked | Dead
    waiting_on: *mut WaitList, // wait list while Blocked
    wait_next: *mut Task,      // link inside that wait list (a timed waiter also uses `next` for the sleep list)
    timed_out: bool,           // set by `tick` when the timeout beat the event
    msg: *mut (),              // queue handoff: points at an Option<T> on this task's stack
    current_priority: u8,      // effective; the scheduler uses this (raised by inheritance)
    original_priority: u8,     // declared priority; never changes
    held: *mut MutexCore,      // mutexes this task owns, linked via MutexCore::next_held
    blocked_mutex: *mut MutexCore, // mutex this task is blocked on, else null
    wake: u64,                 // wake tick while Sleeping
    next: *mut Task,           // link in the one list the task is in
    name: &'static str,
    privileged: bool,          // CONTROL.nPRIV for this task, set in kernel_switch; only idle and test tasks
    syscall_frame: *mut u32,   // frame of the syscall it is blocked in; gets Ok/Timeout in r0 at switch-in
}
```

- App declares stacks as statics (`Stack<N>`, `#[repr(align(32))]`) and builds `Task::new(name, priority, entry: fn() -> !, &STACK)` values in `main` (priority 1..=7). `kernel::start(&mut [Task], sys_hz) -> !`; `sys_hz` comes from the HAL clocks (`app::Board.sys_hz`) so the kernel stays board-agnostic.
- The kernel creates the idle task itself (`Task::idle`, priority 0, 128-word stack).
- 8 priorities, higher = more urgent. Priority 0 reserved for the idle task; apps use 1–7. Ready set = `u8` bitmap + per-priority list; highest via CLZ.
- All task lists are `TaskList<L: Link>` (`collections/list.rs`): intrusive, singly linked, head + tail, with `push_back`, `pop_front`, `insert_sorted`, `remove`. `L` picks the link field (`Next` for ready lists and the sleep list, `WaitNext` for wait lists), so the two kinds cannot be mixed up.
- PendSV (asm, lowest exception priority):
  1. Hardware has stacked R0–R3, R12, LR, PC, xPSR (+ S0–S15, FPSCR if FP active; lazy stacking).
  2. Push R4–R11 + EXC_RETURN to PSP; if EXC_RETURN bit 4 == 0 also push S16–S31.
  3. Store `sp` to current task; call `sched::switch()`.
  4. Load next `sp`, `stack_limit` into PSP/PSPLIM, set CONTROL.nPRIV, (level 4) load MPU regions, pop, `bx` EXC_RETURN.
- Initial frame: xPSR `0x0100_0000`, PC = entry, LR = trap that panics, EXC_RETURN `0xFFFF_FFFD`.
- Start: make every task and idle ready, set PSP to a dummy area, pend PendSV. No special first-task path.
- Stack overflow: PSPLIM → UsageFault (STKOF), which kills the task (section 5).

## 3. Scheduling and time

- Highest-priority ready task runs. A wakeup that outranks the running task pends PendSV immediately.
- Same-priority tasks round-robin with a 1-tick time slice.
- Idle task (priority 0): `loop { wfi() }`.
- SysTick at 1 kHz from the core clock. `u64` tick count, read inside a critical section; never wraps.
- Sleepers sit in a list sorted by wake tick; SysTick only examines the head, wakes due tasks, rotates round-robin, pends PendSV if needed.
- API: `now() -> Instant`, `sleep(Duration)`, `sleep_until(Instant)`, `yield_now()`. `Instant` is a newtype over the `u64` tick count (never wraps); `Duration` is `core::time::Duration`, rounded up to whole ticks so a sleep never ends early.
- Critical section: the kernel provides the `critical-section` implementation (`arch/svc.rs`; defmt and the HAL use it). Privileged code (handlers, `main`, idle) masks interrupts with PRIMASK. An unprivileged task cannot (`cpsid` is ignored), so it makes `Sys::CsEnter`, which raises BASEPRI to `0x80` (masks SysTick and PendSV, not SVCall or faults); `Sys::CsExit` restores it. Restore state: bit 8 set = BASEPRI path, old value in bits 0–7. All scheduler state lives in one `Sched` value in `sched.rs`; `arch::with_sched` (PRIMASK) is the only way to reach it.
- Exception priorities: SysTick `0x80`, PendSV `0xFF`. SysTick can interrupt PendSV, hence the critical section in `kernel_switch`.
- The running task is in no list; `switch()` requeues it at its list's tail only if still `Running` (a task woken before its switch is already queued).
- Host tests: bitmap highest pick, round-robin order, sorted sleep insert with FIFO ties, wake on exact tick, preempt/time-slice decisions, the wake-before-switch race.

## 4. Synchronization

Common:
- Each object has a wait list ordered by priority, FIFO within a priority.
- Blocking calls take `timeout: Option<Duration>` (`None` = forever, `Some(Duration::ZERO)` = never block). Timed waiters are on both the wait list and the sleep list; whichever fires first removes the task from both. Timeout returns `Err(Timeout)`.
- Handoff on wake: the woken task receives the unit/lock/item directly, so no other task can grab it first.

Semaphore (counting): `new(count)`, `take(timeout) -> Result<(), Timeout>`, `give()`. `give()` may be called from interrupt handlers.

`Mutex<T>`: `lock(timeout) -> Result<MutexGuard<T>, Timeout>`; the guard derefs to `T` and unlocks on drop. `MutexGuard` is `!Send`, so only the owner unlocks.
- Kernel half `MutexCore { owner, waiters, next_held }`. Locking again from the owner panics (not recursive).
- Unlock hands the mutex straight to the most urgent waiter (no re-contention).
- Priority inheritance, recomputed rather than undone: `refresh_priority(t)` sets `t` to max(`original_priority`, head waiter of each mutex in `t.held`) and, if that changed and `t` is blocked on a mutex, repeats for that mutex's owner (an iterative chain walk; stops at the first unchanged priority, so it terminates even in a deadlock cycle). Called on block (for the owner), unlock (for the old owner) and timeout (for the owner the waiter left).
- `set_priority` keeps the task in the right list: a Ready task moves to its new `ready[p]`; a Blocked task is re-sorted in its wait list.
- Task context only (`lock` asserts thread mode). Deadlocks are not detected.

`Queue<T, N>`: fixed ring buffer. `send(item, timeout)`, `recv(timeout)` block on full/empty. `try_send`, `try_recv` never block and may be called from interrupt handlers.

Queue storage is `Ring<T, N>` (`collections/ring.rs`, host-tested): `[MaybeUninit<T>; N]` + head + len, drops leftover items. Direct handoff: a sender writes into a blocked receiver's `msg` slot; a sender blocked on full parks its item in its own slot, and the receiver that frees space moves it into the ring. Whoever makes another task ready checks `should_switch()`; whoever blocks itself always yields. Deciding to wait and calling `block_current` happen in one critical section (no lost wakeups).

Host tests: wait-list order, timeout vs. event race, ring wraparound, inheritance (owner boost, handover/restore, chain, boost kept from another held mutex, timed-out waiter withdraws its boost, remaining waiter keeps its boost).

## 5. Protection (level 4)

Syscalls:
- Public API signatures unchanged. Task-side calls execute `svc #N` (`svc::<{ Sys::X as u8 }>`), args in r0–r3 (a `u64` as two words, low first). Timeouts encode as ticks with `u64::MAX` = forever (`Some` is capped at `u64::MAX - 1`).
- `SVCall` (5 asm instructions) picks MSP/PSP from EXC_RETURN bit 2 and tail-jumps to `svc_dispatch(frame)`. The number comes from the `svc` instruction (stacked pc − 2), checked with `Sys::try_from`; an unknown number kills the task. One `with_sched` covers the call and `finish`; the result goes into stacked r0 (`Now` also r1).
- Blocking: the kernel half returns `Status::Blocked`; `finish` stores the frame in `syscall_frame` and pends PendSV whenever the caller stopped running or `should_switch()`. `kernel_switch` writes `Ok`/`Timeout` (from `timed_out`) into that frame when the task is switched back in, so one syscall returns the final answer.
- Interrupt handlers are privileged and must not `svc` (it would escalate to HardFault): `give`, `try_send`, `try_recv`, `now`, `yield_now` check IPSR (`in_handler`) and use `from_handler` / direct paths. Syscalls run at priority 0 and must stay short; slow work belongs in a task.
- Generic queue: `Queue<T, N>` is `repr(C)` and starts with `&'static QueueOps` (send/recv compiled for its `T`); the dispatcher calls through it. Safe to call only once M6 checks that object pointers lie in `.kobj`.
- Tasks are unprivileged; `kernel_switch` sets CONTROL.nPRIV per task. Only the idle task and board-test tasks (`Task::new_privileged`, kernel feature `test-hooks`, enabled by `app`) run privileged.

MPU (ARMv8-M PMSA, 8 regions, base/limit at 32-byte granularity, PRIVDEFENA = 1):
- Per task: flash RX; own stack RW + XN; the `.shared` RW region (holds all task-shared data, including every `Mutex<T>`'s `T`); peripheral ranges listed in the task declaration (at most 5 regions remain for these).
- Regions reloaded in PendSV on each switch.
- Kernel objects (sync objects, wait lists, TCBs) live in `.kobj`, privileged-only. Syscalls validate pointers: object pointers must lie inside `.kobj`; buffer pointers must lie inside the caller's regions.

Fault policy (M5): MemManage, BusFault and UsageFault are enabled with their own handlers (`arch/fault.rs`).
- Fault that interrupted a task (ICSR.RETTOBASE) other than idle: reset BASEPRI, log task name + kind + CFSR, clear CFSR, `kill_current` (`Dead`; `switch` never requeues it), move PSP to scratch (`DUMMY`, PSPLIM 0 first, so PendSV can save the dead task's registers even after a stack overflow), pend PendSV.
- Fault in a handler, in `main` or in idle: panic (exit 1).
- A killed task's mutexes stay locked; their waiters block until their timeout.
- `app` has its own panic handler (defmt log + semihosting exit 1): panic-probe ends with `udf`, which would now only kill the panicking task, and it writes SHCSR, which faults when unprivileged.

## 6. Testing and milestones

- Host: `cargo ktest` (alias for `cargo test -p kernel --target x86_64-unknown-linux-gnu`) for pure logic.
- Board: one binary per milestone in `app/src/bin/`, run via `probe-rs run`. Tasks verify with `assert!`; a panic anywhere ends the run with exit 1 (`app`'s panic handler). Success ends with a semihosting exit (`app::exit_ok()`, exit 0). Verified 2026-10-14: `bkpt` exits 1, so it is not used.

| # | Goal | Proof |
|---|---|---|
| M0 | Workspace, `rp235x-hal` boot, `defmt` | GP15 LED blinks via busy-wait; pass/fail exit mechanism confirmed |
| M1 | Cooperative context switch | Two tasks alternate logs via `yield_now` |
| M2 | SysTick, priorities, sleep, idle | Sleep timing within 1 tick; higher-priority wakeup preempts immediately |
| M3 | Semaphore, queue, interrupt → task | TIMER0 alarm IRQ `give()`s to a task (worst wakeup latency 11–12 µs; a timer replaces the planned GPIO button so the test runs unattended); producer/consumer over a 4-slot queue incl. full/empty timeouts |
| M4 | Mutex + priority inheritance | Low/medium/high inversion: H waits 8 ms (L's critical section) instead of ~200 ms (mutant without inheritance: 201 ms, fails); two time-sliced tasks lose no updates (200/200); try-lock and timed lock fail on a held mutex |
| M5 | SVC + unprivileged tasks, task kill | All M1–M4 tests pass with unprivileged tasks (IRQ→task latency 12 µs). `m5_fault`: a SysTick write (BusFault, CFSR `00008200`), a stack overflow (`00100000`) and an undefined instruction while holding a mutex (`00010000`) each kill only their task; SysTick unchanged, the innocent task ran 10 times in 10 ms, the dead task's mutex stays locked. Mutants: tasks left privileged, no r0 writeback, no-op kill each fail a test |
| M6 | MPU | Bad pointer kills only the offending task; others keep running |
| M7 | Multicore (own spec) | Tasks run on both cores; a `give()` on one core wakes a task on the other |
| M8 | Hazard3 RISC-V port (own spec) | `arch/` splits into `arch/cortex_m33/` and `arch/hazard3/` behind the same interface; one `--target` flag picks the core. All board tests that don't touch Arm-only registers pass on Hazard3, multicore included: context switch saves all registers in software, `ecall` syscalls, machine timer tick, PMP instead of MPU. Wi-Fi not required on RISC-V |
