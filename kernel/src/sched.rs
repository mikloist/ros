//! Priority scheduler: ready lists, sleep list, tick. Pure logic, host-tested.
//!
//! Invariants:
//! - The running task (`current`) is in no list.
//! - Bit `p` of `bitmap` is set iff `ready[p]` is non-empty.
//! - `sleeping` is sorted by `wake` ascending; equal `wake`s keep arrival order.
//! - A task is in at most one list at a time, linked through `Task::next`.
use core::ptr::null_mut;

use crate::task::{State, Task};

pub(crate) const PRIOS: usize = 8;

/// FIFO of tasks linked through `Task::next`.
#[derive(Clone, Copy)]
struct List {
    head: *mut Task,
    tail: *mut Task,
}

impl List {
    const EMPTY: List = List {
        head: null_mut(),
        tail: null_mut(),
    };

    fn is_empty(&self) -> bool {
        self.head == null_mut()
    }

    /// Appends `t`. Caller guarantees `t` is valid and in no list.
    unsafe fn push_back(&mut self, t: *mut Task) {
        unsafe {
            (*t).next = null_mut();
            if self.tail == null_mut() {
                self.head = t;
                self.tail = t;
            } else {
                (*self.tail).next = t;
                self.tail = t;
            }
        }
    }

    /// Removes and returns the head. Caller guarantees the list is non-empty.
    unsafe fn pop_front(&mut self) -> *mut Task {
        let current = self.head;
        unsafe {
            let next = (*self.head).next;
            if next == null_mut() {
                self.tail = null_mut();
                self.head = null_mut();
            } else {
                self.head = next;
            }
        }
        return current;
    }
}

pub(crate) struct Sched {
    ready: [List; PRIOS],
    bitmap: u8,
    sleeping: *mut Task,
    current: *mut Task,
    now: u64,
}

impl Sched {
    pub(crate) const fn new() -> Self {
        Self {
            ready: [List::EMPTY; PRIOS],
            bitmap: 0,
            sleeping: null_mut(),
            current: null_mut(),
            now: 0,
        }
    }

    /// Ticks since start.
    pub(crate) fn now(&self) -> u64 {
        return self.now;
    }

    /// The running task; null before the first `switch`.
    pub(crate) fn current(&self) -> *mut Task {
        return self.current;
    }

    /// Marks `t` Ready and appends it to the list for its `current_priority`.
    ///
    /// # Safety
    /// `t` is valid for the scheduler's lifetime and in no list.
    pub(crate) unsafe fn make_ready(&mut self, t: *mut Task) {
        unsafe {
            let prio = (*t).current_priority;
            self.ready[prio as usize].push_back(t);
            self.bitmap |= 1 << prio;
            (*t).state = State::Ready;
        }
    }

    /// Highest priority with a ready task, if any (hint: `leading_zeros`).
    fn highest(&self) -> Option<u8> {
        if self.bitmap == 0 {
            return None;
        }

        return Some(7 - self.bitmap.leading_zeros() as u8);
    }

    /// Called from PendSV. If the current task is still `Running`, puts it back
    /// at the tail of its list; then pops the head of the highest non-empty
    /// list, marks it `Running`, makes it `current` and returns it.
    pub(crate) fn switch(&mut self) -> *mut Task {
        if self.current != null_mut() {
            unsafe {
                let current = self.current;
                if (*current).state == State::Running {
                    self.make_ready(current);
                }
            }
        }

        let highest = self.highest();
        highest.expect("no ready task, idle is missing ?");
        unsafe {
            let current_task = self.ready[highest.unwrap() as usize].pop_front();
            (*current_task).state = State::Running;
            self.current = current_task;
            if self.ready[highest.unwrap() as usize].is_empty() {
                self.bitmap &= !(1 << highest.unwrap());
            }
            return current_task;
        }
    }

    /// Puts the current task to sleep until tick `until` (sorted into
    /// `sleeping`). Returns false and does nothing if `until <= now`.
    /// The caller pends PendSV when this returns true.
    pub(crate) fn sleep_current(&mut self, until: u64) -> bool {
        if until <= self.now {
            return false;
        }
        unsafe {
            let cur = self.current;
            assert!(
                !cur.is_null() && (*cur).state == State::Running,
                "sleep_current: no running task"
            );

            (*cur).state = State::Sleeping;
            (*cur).wake = until;

            let mut link: *mut *mut Task = &mut self.sleeping;
            while !(*link).is_null() && (**link).wake <= until {
                link = &mut (**link).next;
            }
            (*cur).next = *link;
            *link = cur;
        }

        return true;
    }

    /// Called from SysTick. Advances `now`, makes every sleeper with
    /// `wake <= now` ready, and returns true if a ready task's priority is
    /// >= the current task's (preempt, or time slice over). False before the
    /// first switch.
    pub(crate) fn tick(&mut self) -> bool {
        self.now += 1;
        unsafe {
            while !self.sleeping.is_null() && (*self.sleeping).wake <= self.now {
                let c = self.sleeping;
                self.sleeping = (*c).next;
                self.make_ready(c);
            }
            if self.current.is_null() {
                return false;
            }
            return Some((*self.current).current_priority) <= self.highest();
        }
    }
}

#[cfg(test)]
mod tests;
