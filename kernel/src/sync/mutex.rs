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

/// A mutex protecting a `T`. Declare it with `kernel::mutex!`: the lock
/// state (`RawMutex`) goes in `.kobj`, where only the kernel can touch it,
/// and the data (`MutexData`) in shared RAM, where the owner reaches it
/// through the guard. The `Mutex` itself is just the two references.
pub struct Mutex<T: 'static> {
    core: &'static RawMutex,
    data: &'static MutexData<T>,
}

/// Kernel half of a mutex. Public only for `kernel::mutex!`.
#[doc(hidden)]
pub struct RawMutex(UnsafeCell<MutexCore>);

// SAFETY: only touched by the kernel halves, with the scheduler locked.
unsafe impl Sync for RawMutex {}

impl RawMutex {
    pub const fn new() -> Self {
        Self(UnsafeCell::new(MutexCore::new()))
    }
}

/// The protected value. Public only for `kernel::mutex!`.
#[doc(hidden)]
pub struct MutexData<T>(UnsafeCell<T>);

// SAFETY: reached only through a guard, which exists for one task at a time.
unsafe impl<T: Send> Sync for MutexData<T> {}

impl<T> MutexData<T> {
    pub const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }
}

/// Unlocks on drop. `!Send`: only the owner may unlock.
pub struct MutexGuard<'a, T: 'static> {
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
        unsafe { &*self.mutex.data.0.get() }
    }
}

impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as above; `&mut self` rules out other borrows through this guard.
        unsafe { &mut *self.mutex.data.0.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        svc::<{ Sys::MutexUnlock as u8 }>(self.mutex.core.0.get() as u32, 0, 0, 0);
    }
}

impl<T> Mutex<T> {
    /// Joins the two halves. Use `kernel::mutex!` instead.
    #[doc(hidden)]
    pub const fn from_parts(core: &'static RawMutex, data: &'static MutexData<T>) -> Self {
        Self { core, data }
    }

    /// Task context only.
    pub fn lock(&self, timeout: Option<Duration>) -> Result<MutexGuard<'_, T>, Timeout> {
        assert!(!in_handler(), "Mutex::lock from an interrupt");
        let (lo, hi) = split(timeout_to_ticks(timeout));
        result(svc::<{ Sys::MutexLock as u8 }>(self.core.0.get() as u32, lo, hi, 0).0)?;
        Ok(MutexGuard::new(self))
    }
}
