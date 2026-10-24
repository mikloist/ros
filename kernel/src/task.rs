use core::cell::UnsafeCell;

use crate::region::{PERIPHERAL_REGIONS, Region};

use crate::sched::{Instant, MutexCore, PRIOS, WaitList};

#[repr(C, align(32))] // 8-byte aligned frames (AAPCS); 32 for the MPU in M6
pub struct Stack<const N: usize> {
    stack: UnsafeCell<[u32; N]>,
}

// SAFETY: each `Stack` is handed to exactly one `Task::new`; only that task
// (and the kernel on its behalf) ever touches the words inside.
unsafe impl<const N: usize> Sync for Stack<N> {}

impl<const N: usize> Stack<N> {
    pub const fn new() -> Self {
        const {
            assert!(
                N % 8 == 0,
                "stack size must be a multiple of 8 words (32 bytes, MPU granule)"
            )
        };
        const { assert!(N >= 64, "stack must be at least 64 words") };
        return Self {
            stack: UnsafeCell::new([0; N]),
        };
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum State {
    Ready,
    Running,
    Sleeping,
    Blocked,
    Dead,
}

pub struct Task {
    pub(crate) sp: *mut u32,
    pub(crate) stack_end: *mut u32,
    pub(crate) stack_limit: *mut u32,
    pub(crate) state: State,
    pub(crate) next: *mut Task,
    pub(crate) name: &'static str,
    pub(crate) current_priority: u8,
    pub(crate) original_priority: u8,
    pub(crate) wake: Instant,
    pub(crate) waiting_on: *mut WaitList,
    pub(crate) wait_next: *mut Task,
    pub(crate) timed_out: bool,
    pub(crate) msg: *mut (),
    pub(crate) held: *mut MutexCore,
    pub(crate) blocked_mutex: *mut MutexCore,
    pub(crate) privileged: bool,
    pub(crate) syscall_frame: *mut u32,
    pub(crate) peripherals: [Region; PERIPHERAL_REGIONS],
}

fn task_exit() -> ! {
    panic!("Task returned")
}

impl Task {
    /// An application task. Priority 0 is reserved for the kernel's idle task.
    pub fn new<const N: usize>(
        name: &'static str,
        priority: u8,
        entry: fn() -> !,
        stack: &'static Stack<N>,
    ) -> Task {
        assert!(
            (1..PRIOS).contains(&(priority as usize)),
            "task priority must be 1..={} (0 is idle)",
            PRIOS - 1
        );
        Self::build(name, priority, entry, stack)
    }

    /// The kernel's idle task: priority 0, always ready when nothing else is.
    pub(crate) fn idle<const N: usize>(entry: fn() -> !, stack: &'static Stack<N>) -> Task {
        let mut t = Self::build("idle", 0, entry, stack);
        t.privileged = true;
        return t;
    }

    /// Writes the initial frame and fills the fields; callers check `priority`.
    fn build<const N: usize>(
        name: &'static str,
        priority: u8,
        entry: fn() -> !,
        stack: &'static Stack<N>,
    ) -> Task {
        // SAFETY: see `impl Sync for Stack`: this is the stack's only user.
        let words: &mut [u32; N] = unsafe { &mut *stack.stack.get() };
        let start = init_stack(
            words,
            entry as fn() -> ! as usize as u32,
            task_exit as fn() -> ! as usize as u32,
        );
        let base = words.as_mut_ptr();
        unsafe {
            Self {
                sp: base.add(start),
                stack_limit: base,
                stack_end: base.add(N),
                state: State::Ready,
                next: core::ptr::null_mut(),
                name: name,
                current_priority: priority,
                original_priority: priority,
                wake: Instant::ZERO,
                waiting_on: core::ptr::null_mut(),
                wait_next: core::ptr::null_mut(),
                timed_out: false,
                msg: core::ptr::null_mut(),
                held: core::ptr::null_mut(),
                blocked_mutex: core::ptr::null_mut(),
                privileged: false,
                syscall_frame: core::ptr::null_mut(),
                peripherals: [Region::DISABLED; PERIPHERAL_REGIONS],
            }
        }
    }

    pub fn with_peripheral(mut self, base: usize, len: usize) -> Task {
        let i = self.peripherals.iter().position(|r| *r == Region::DISABLED);
        let slot = i.expect("a task can be granted at most 5 peripheral ranges");
        self.peripherals[slot] = Region::device(base, len);
        self
    }

    pub(crate) fn stack_contains(&self, p: usize, len: usize) -> bool {
        let (lo, hi) = (self.stack_limit as usize, self.stack_end as usize);
        p >= lo && p.checked_add(len).is_some_and(|end| end <= hi)
    }
    /// Board tests only: like `new`, but the task runs privileged (it may
    /// touch core peripherals such as the NVIC or SysTick).
    #[cfg(feature = "test-hooks")]
    pub fn new_privileged<const N: usize>(
        name: &'static str,
        priority: u8,
        entry: fn() -> !,
        stack: &'static Stack<N>,
    ) -> Self {
        let mut s = Self::new(name, priority, entry, stack); // same priority check as `new`
        s.privileged = true;
        return s;
    }
}

#[cfg(test)]
impl Task {
    /// Stackless task for scheduler tests: only the fields `Sched` reads are meaningful.
    pub(crate) fn for_test(prio: u8) -> Task {
        Task {
            sp: core::ptr::null_mut(),
            stack_limit: core::ptr::null_mut(),
            state: State::Ready,
            current_priority: prio,
            original_priority: prio,
            wake: Instant::ZERO,
            waiting_on: core::ptr::null_mut(),
            wait_next: core::ptr::null_mut(),
            timed_out: false,
            msg: core::ptr::null_mut(),
            held: core::ptr::null_mut(),
            blocked_mutex: core::ptr::null_mut(),
            privileged: false,
            syscall_frame: core::ptr::null_mut(),
            on_cpu: false,
            stack_end: core::ptr::null_mut(),
            peripherals: [Region::DISABLED; PERIPHERAL_REGIONS],
            next: core::ptr::null_mut(),
            name: "t",
        }
    }
}

/// Exception return value for a fresh task: return to Thread mode, Secure,
/// use PSP, standard (non-FP) frame. Bit 4 = 1 means "no FP frame".
const EXC_RETURN: u32 = 0xFFFF_FFFD;
/// Initial xPSR: only the Thumb bit (24). Cortex-M runs Thumb only; returning
/// with it clear faults (INVSTATE).
const XPSR: u32 = 0x0100_0000;

pub(crate) const FRAME_WORDS: usize = 17;
pub(crate) fn init_stack(stack: &mut [u32], entry: u32, on_exit: u32) -> usize {
    assert!(
        stack.len() >= FRAME_WORDS,
        "Stack has to be bigger {FRAME_WORDS}"
    );

    let s = stack.len() - FRAME_WORDS;
    stack[s..s + 16].fill(0);
    stack[s + 8] = EXC_RETURN;
    stack[s + 14] = on_exit;
    stack[s + 15] = entry & !1;
    stack[s + 16] = XPSR;
    return s;
}

#[cfg(test)]
mod tests;
