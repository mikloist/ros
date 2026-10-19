//! Semaphore, Queue, Mutex. Each operation has a kernel half (runs inside
//! `svc_dispatch` or `from_handler`, scheduler already locked, returns a
//! `Status`) and a task half (encodes the arguments and makes the syscall).
mod mutex;
mod queue;
mod semaphore;

pub use mutex::{Mutex, MutexGuard};
pub(crate) use mutex::{mutex_lock, mutex_unlock};
pub use queue::Queue;
pub(crate) use queue::QueueOps;
pub use semaphore::Semaphore;
pub(crate) use semaphore::{SemInner, sem_give, sem_take};

use crate::arch::svc::Status;

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub struct Timeout;

/// Task side: turns the syscall's r0 back into a `Result`.
fn result(r0: u32) -> Result<(), Timeout> {
    if r0 == Status::Ok as u32 { Ok(()) } else { Err(Timeout) }
}
