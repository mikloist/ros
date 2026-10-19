//! Mutex with priority inheritance (the inheritance itself is in `sched.rs`).
use core::time::Duration;
use core::{cell::UnsafeCell, marker::PhantomData};

use super::{Timeout, result};
use crate::arch::svc::{Status, Sys, in_handler, split, svc};
use crate::sched::{Lock, MutexCore, Sched, timeout_to_ticks};

/// Kernel half of `lock`.
pub(crate) unsafe fn mutex_lock(s: &mut Sched, m: *mut MutexCore, timeout: Option<Duration>) -> Status {
    match unsafe { s.lock_current(m, timeout) } {
        Lock::Acquired => Status::Ok,
        Lock::WouldBlock => Status::Timeout,
        Lock::Blocked => Status::Blocked,
    }
}

/// Kernel half of unlock (guard drop). Whether to switch is the dispatcher's call.
pub(crate) unsafe fn mutex_unlock(s: &mut Sched, m: *mut MutexCore) -> Status {
    unsafe { s.unlock_current(m) };
    Status::Ok
}

pub struct Mutex<T> {
    core: UnsafeCell<MutexCore>,
    data: UnsafeCell<T>,
}

/// Unlocks on drop. `!Send`: only the owner may unlock.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
    _not_send: PhantomData<*const ()>,
}

impl<'a, T> MutexGuard<'a, T> {
    const fn new(mutex: &'a Mutex<T>) -> Self {
        Self {
            mutex,
            _not_send: PhantomData,
        }
    }
}

impl<T> core::ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard exists only while this task owns the mutex.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as above; `&mut self` rules out other borrows through this guard.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        svc::<{ Sys::MutexUnlock as u8 }>(self.mutex.core.get() as u32, 0, 0, 0);
    }
}

// SAFETY: `core` is only touched by the kernel halves; `data` only through a
// guard, which exists for one task at a time.
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            core: UnsafeCell::new(MutexCore::new()),
            data: UnsafeCell::new(data),
        }
    }

    /// Task context only.
    pub fn lock(&self, timeout: Option<Duration>) -> Result<MutexGuard<'_, T>, Timeout> {
        assert!(!in_handler(), "Mutex::lock from an interrupt");
        let (lo, hi) = split(timeout_to_ticks(timeout));
        result(svc::<{ Sys::MutexLock as u8 }>(self.core.get() as u32, lo, hi, 0).0)?;
        Ok(MutexGuard::new(self))
    }
}
