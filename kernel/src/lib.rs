//! ros kernel. Pure logic is host-testable; `arch` and `sync` are Cortex-M33 only.
#![cfg_attr(not(test), no_std)]

mod ring;
mod sched;
mod task;
pub use core::time::Duration;
pub use sched::Instant;
pub use task::{Stack, Task};

#[cfg(target_arch = "arm")]
mod arch;
#[cfg(target_arch = "arm")]
mod sync;
#[cfg(target_arch = "arm")]
pub use arch::{now, sleep, sleep_until, start, yield_now};
#[cfg(target_arch = "arm")]
pub use sync::{Mutex, MutexGuard, Queue, Semaphore, Timeout};
