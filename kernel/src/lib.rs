//! ros kernel. Pure logic is host-testable; `arch` is Cortex-M33 only.
#![cfg_attr(not(test), no_std)]

mod task;
pub use task::{Stack, Task};

#[cfg(target_arch = "arm")]
mod arch;
#[cfg(target_arch = "arm")]
pub use arch::{start, yield_now};
