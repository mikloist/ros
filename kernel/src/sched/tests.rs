//! Host tests for the scheduler. Run: `cargo ktest`.
use super::*;

fn at(ticks: u64) -> Instant {
    Instant::from_ticks(ticks)
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Owns the test tasks; frees them when the test ends. The scheduler only ever
/// sees raw pointers, so they must come from `Box::into_raw` (not `&mut`).
struct Tasks(Vec<*mut Task>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for &t in &self.0 {
            drop(unsafe { Box::from_raw(t) });
        }
    }
}

/// Scheduler with `prios` made ready in order, then an idle task (prio 0).
/// Returns (sched, idle, tasks in `prios` order, owner).
fn sched_with(prios: &[u8]) -> (Sched, *mut Task, Vec<*mut Task>, Tasks) {
    let new = |p| Box::into_raw(Box::new(Task::for_test(p)));
    let ts: Vec<_> = prios.iter().map(|&p| new(p)).collect();
    let idle = new(0);
    let mut s = Sched::new();
    unsafe {
        for &t in &ts {
            s.make_ready(t);
        }
        s.set_idle(0, idle);
    }
    let mut all = ts.clone();
    all.push(idle);
    (s, idle, ts, Tasks(all))
}

#[test]
fn picks_highest_priority_ready_task() {
    let (mut s, _, t, _own) = sched_with(&[1, 3, 2]);
    assert_eq!(s.switch(), t[1]);
}

#[test]
fn higher_priority_keeps_cpu_over_lower() {
    let (mut s, _, t, _own) = sched_with(&[1, 3]);
    assert_eq!(s.switch(), t[1]);
    assert_eq!(s.switch(), t[1], "yielding to a lower priority task");
}

#[test]
fn equal_priorities_take_turns_in_fifo_order() {
    let (mut s, _, t, _own) = sched_with(&[2, 2, 2]);
    let order: Vec<_> = (0..6).map(|_| s.switch()).collect();
    assert_eq!(order, [t[0], t[1], t[2], t[0], t[1], t[2]]);
}

#[test]
fn idle_runs_when_every_task_sleeps() {
    let (mut s, idle, t, _own) = sched_with(&[1]);
    assert_eq!(s.switch(), t[0]);
    assert!(s.sleep_current(at(5)));
    assert_eq!(s.switch(), idle);
}

#[test]
fn sleep_until_now_or_past_does_not_sleep() {
    let (mut s, _, t, _own) = sched_with(&[1]);
    s.switch();
    s.tick();
    s.tick();
    assert!(!s.sleep_current(at(2)), "deadline == now");
    assert!(!s.sleep_current(at(1)), "deadline in the past");
    assert_eq!(s.switch(), t[0], "task should still be runnable");
}

#[test]
fn sleeper_wakes_on_its_tick_and_not_before() {
    let (mut s, idle, t, _own) = sched_with(&[1]);
    s.switch();
    s.sleep_current(at(3));
    assert_eq!(s.switch(), idle);
    assert!(!s.tick(), "tick 1: nothing woke");
    assert!(!s.tick(), "tick 2: nothing woke");
    assert!(s.tick(), "tick 3: task woke and outranks idle");
    assert_eq!(s.now(), at(3));
    assert_eq!(s.switch(), t[0]);
}

#[test]
fn sleepers_wake_by_deadline_then_fifo_on_ties() {
    let (mut s, idle, t, _own) = sched_with(&[1, 1, 1]);
    assert_eq!(s.switch(), t[0]);
    s.sleep_current(at(5));
    assert_eq!(s.switch(), t[1]);
    s.sleep_current(at(3));
    assert_eq!(s.switch(), t[2]);
    s.sleep_current(at(5));
    assert_eq!(s.switch(), idle);
    for _ in 0..5 {
        s.tick();
    }
    let order: Vec<_> = (0..3).map(|_| s.switch()).collect();
    assert_eq!(order, [t[1], t[0], t[2]]);
}

#[test]
fn tick_requests_switch_only_for_an_equal_or_higher_peer() {
    let (mut s, _, _, _own) = sched_with(&[2, 2]);
    s.switch();
    assert!(s.tick(), "same-priority peer ready: time slice is up");

    let (mut s, _, _, _own) = sched_with(&[2]);
    s.switch();
    assert!(!s.tick(), "only idle (lower) is ready");
}

#[test]
fn tick_requests_preemption_when_higher_task_wakes() {
    let (mut s, _, t, _own) = sched_with(&[3, 1]);
    assert_eq!(s.switch(), t[0]);
    s.sleep_current(at(2));
    assert_eq!(s.switch(), t[1]);
    assert!(!s.tick());
    assert!(s.tick(), "high task woke above the running one");
    assert_eq!(s.switch(), t[0]);
}

#[test]
fn task_woken_before_its_switch_is_not_queued_twice() {
    // Race: a task calls sleep, but SysTick wakes it before PendSV runs.
    let (mut s, idle, t, _own) = sched_with(&[1]);
    assert_eq!(s.switch(), t[0]);
    s.sleep_current(at(1));
    s.tick(); // wakes t[0] while it is still `current`
    assert_eq!(s.switch(), t[0]);
    s.sleep_current(at(10));
    assert_eq!(
        s.switch(),
        idle,
        "a stale second queue entry ran a sleeping task"
    );
}

#[test]
fn tick_before_start_requests_nothing() {
    let (mut s, _, _, _own) = sched_with(&[1]);
    assert!(!s.tick());
}

// ---- M3: wait lists and timeouts ----

fn wait_list() -> Box<WaitList> {
    Box::new(WaitList::new())
}

#[test]
fn waiters_wake_most_urgent_first_then_fifo() {
    let (mut s, _, t, _own) = sched_with(&[1, 3, 1]);
    let mut wl = wait_list();
    for &want in &[t[1], t[0], t[2]] {
        assert_eq!(s.switch(), want);
        unsafe { s.block_current(&mut *wl, None) };
    }
    let order: Vec<_> = (0..4).map(|_| unsafe { s.wake_one(&mut *wl) }).collect();
    assert_eq!(order, [t[1], t[0], t[2], std::ptr::null_mut()]);
    assert!(wl.is_empty());
}

#[test]
fn timed_wait_expires_on_its_tick() {
    let (mut s, idle, t, _own) = sched_with(&[1]);
    let mut wl = wait_list();
    s.switch();
    unsafe { s.block_current(&mut *wl, Some(ms(3))) };
    assert_eq!(s.switch(), idle);
    assert!(!s.tick());
    assert!(!s.tick());
    assert!(s.tick(), "tick 3: timeout woke the task");
    assert!(wl.is_empty(), "timed-out task still on the wait list");
    assert_eq!(s.switch(), t[0]);
    assert!(unsafe { (*t[0]).timed_out });
}

#[test]
fn event_cancels_the_timeout() {
    let (mut s, idle, t, _own) = sched_with(&[1]);
    let (mut a, mut b) = (wait_list(), wait_list());
    s.switch();
    unsafe { s.block_current(&mut *a, Some(ms(5))) };
    s.switch();
    assert_eq!(unsafe { s.wake_one(&mut *a) }, t[0]);
    assert_eq!(s.switch(), t[0]);
    assert!(!unsafe { (*t[0]).timed_out });
    // Now wait forever elsewhere: the old 5-tick timer must not fire.
    unsafe { s.block_current(&mut *b, None) };
    assert_eq!(s.switch(), idle);
    for _ in 0..10 {
        s.tick();
    }
    assert_eq!(s.switch(), idle, "a cancelled timeout woke the task");
    assert_eq!(unsafe { s.wake_one(&mut *b) }, t[0]);
}

#[test]
fn untimed_wait_never_times_out() {
    let (mut s, idle, _, _own) = sched_with(&[1]);
    let mut wl = wait_list();
    s.switch();
    unsafe { s.block_current(&mut *wl, None) };
    s.switch();
    for _ in 0..1000 {
        s.tick();
    }
    assert_eq!(s.switch(), idle);
    assert!(!wl.is_empty());
}

#[test]
fn waking_a_more_urgent_waiter_requests_a_switch() {
    let (mut s, _, t, _own) = sched_with(&[3, 1]);
    let mut wl = wait_list();
    assert_eq!(s.switch(), t[0]);
    unsafe { s.block_current(&mut *wl, None) };
    assert_eq!(s.switch(), t[1]);
    assert!(!s.should_switch());
    unsafe { s.wake_one(&mut *wl) };
    assert!(s.should_switch(), "woke priority 3 while priority 1 runs");
}

#[test]
fn waking_a_less_urgent_waiter_does_not_switch() {
    let (mut s, _, t, _own) = sched_with(&[1, 3]);
    let mut wl = wait_list();
    assert_eq!(s.switch(), t[1]);
    // t[1] must step aside so t[0] can block, then come back.
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[0]);
    unsafe { s.block_current(&mut *wl, None) };
    s.switch();
    s.tick();
    assert_eq!(s.switch(), t[1]);
    unsafe { s.wake_one(&mut *wl) };
    assert!(!s.should_switch());
}

#[test]
fn waiter_woken_before_its_switch_is_not_queued_twice() {
    // Race: a task blocks, but an interrupt gives before PendSV runs.
    let (mut s, idle, t, _own) = sched_with(&[1]);
    let mut wl = wait_list();
    assert_eq!(s.switch(), t[0]);
    unsafe {
        s.block_current(&mut *wl, None);
        s.wake_one(&mut *wl);
    }
    assert_eq!(s.switch(), t[0]);
    s.sleep_current(at(10));
    assert_eq!(
        s.switch(),
        idle,
        "a stale second queue entry ran a sleeping task"
    );
}

#[test]
fn instant_math_rounds_durations_up_to_whole_ticks() {
    assert_eq!(
        at(10) + Duration::from_micros(1),
        at(11),
        "a sleep must never end early"
    );
    assert_eq!(at(10) + ms(5), at(15));
    assert_eq!(at(10) - ms(3), at(7));
    assert_eq!(at(2) - ms(5), at(0), "saturates at start");
    assert_eq!(at(15) - at(10), ms(5));
    assert_eq!(
        at(10) - at(15),
        ms(0),
        "earlier minus later is zero, not a wrap"
    );
}

// ---- M4: mutex and priority inheritance ----

fn mutex() -> Box<MutexCore> {
    Box::new(MutexCore::new())
}

fn prio(t: *mut Task) -> u8 {
    unsafe { (*t).current_priority }
}

#[test]
fn free_mutex_is_taken_and_handed_back() {
    let (mut s, _, t, _own) = sched_with(&[1]);
    let mut m = mutex();
    s.switch();
    assert_eq!(unsafe { s.lock_current(&mut *m, None) }, Lock::Acquired);
    assert_eq!(m.owner(), t[0]);
    assert!(!unsafe { s.unlock_current(&mut *m) });
    assert!(m.owner().is_null());
}

#[test]
fn zero_timeout_on_held_mutex_does_not_block() {
    let (mut s, _, t, _own) = sched_with(&[2, 1]);
    let mut m = mutex();
    assert_eq!(s.switch(), t[0]);
    unsafe { s.lock_current(&mut *m, None) };
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[1]);
    assert_eq!(
        unsafe { s.lock_current(&mut *m, Some(Duration::ZERO)) },
        Lock::WouldBlock
    );
    assert_eq!(prio(t[0]), 2, "a try-lock must not boost the owner");
}

#[test]
#[should_panic(expected = "mutex locked twice by the same task")]
fn locking_twice_from_the_same_task_panics() {
    let (mut s, _, _, _own) = sched_with(&[1]);
    let mut m = mutex();
    s.switch();
    unsafe {
        s.lock_current(&mut *m, None);
        s.lock_current(&mut *m, None);
    }
}

/// Classic inversion: L owns the mutex, M is ready, H blocks on the mutex.
/// Returns (sched, [L, M, H], mutex).
fn inversion() -> (Sched, Vec<*mut Task>, Box<MutexCore>, Tasks) {
    let (mut s, _, t, own) = sched_with(&[1, 2, 3]);
    let mut m = mutex();
    // H and M step aside so L can run and take the mutex.
    assert_eq!(s.switch(), t[2]);
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[1]);
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[0]);
    unsafe { s.lock_current(&mut *m, None) };
    // Both wake; H preempts L and blocks on the mutex.
    assert!(s.tick());
    assert_eq!(s.switch(), t[2]);
    assert_eq!(unsafe { s.lock_current(&mut *m, None) }, Lock::Blocked);
    (s, t, m, own)
}

#[test]
fn owner_inherits_the_waiters_priority_and_runs_before_medium() {
    let (mut s, t, _m, _own) = inversion();
    assert_eq!(prio(t[0]), 3, "L must run at H's priority");
    assert_eq!(
        s.switch(),
        t[0],
        "M ran while H waited for L: priority inversion"
    );
}

#[test]
fn unlock_hands_over_and_restores_the_owner() {
    let (mut s, t, mut m, _own) = inversion();
    assert_eq!(s.switch(), t[0]);
    assert!(
        unsafe { s.unlock_current(&mut *m) },
        "H outranks L now: switch"
    );
    assert_eq!(m.owner(), t[2], "handed straight to the waiter");
    assert_eq!(prio(t[0]), 1, "L back to its own priority");
    assert_eq!(s.switch(), t[2]);
    assert_eq!(s.switch(), t[2], "H keeps the CPU");
}

#[test]
fn inheritance_follows_a_chain_of_mutexes() {
    // L holds A. M holds B and blocks on A. H blocks on B: L must reach 3.
    let (mut s, _, t, _own) = sched_with(&[1, 2, 3]);
    let (mut a, mut b) = (mutex(), mutex());
    assert_eq!(s.switch(), t[2]);
    s.sleep_current(at(2));
    assert_eq!(s.switch(), t[1]);
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[0]);
    unsafe { s.lock_current(&mut *a, None) };
    s.tick(); // M wakes
    assert_eq!(s.switch(), t[1]);
    unsafe {
        s.lock_current(&mut *b, None);
        s.lock_current(&mut *a, None); // M blocks on A: L -> 2
    }
    assert_eq!(prio(t[0]), 2);
    s.switch();
    s.tick(); // H wakes
    assert_eq!(s.switch(), t[2]);
    unsafe { s.lock_current(&mut *b, None) }; // H blocks on B: M -> 3, then L -> 3
    assert_eq!(prio(t[1]), 3);
    assert_eq!(prio(t[0]), 3, "boost did not propagate along the chain");
}

#[test]
fn owner_keeps_the_boost_of_mutexes_it_still_holds() {
    // L holds A and B; M waits on B, H waits on A. Unlocking A drops L to 2, not 1.
    let (mut s, _, t, _own) = sched_with(&[1, 2, 3]);
    let (mut a, mut b) = (mutex(), mutex());
    assert_eq!(s.switch(), t[2]);
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[1]);
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[0]);
    unsafe {
        s.lock_current(&mut *a, None);
        s.lock_current(&mut *b, None);
    }
    s.tick();
    assert_eq!(s.switch(), t[2]);
    unsafe { s.lock_current(&mut *a, None) };
    assert_eq!(s.switch(), t[0], "boosted L outranks M");
    s.sleep_current(at(2)); // L steps aside so M can block on B
    assert_eq!(s.switch(), t[1]);
    unsafe { s.lock_current(&mut *b, None) };
    assert_eq!(prio(t[0]), 3);
    s.switch();
    s.tick(); // L wakes
    assert_eq!(s.switch(), t[0]);
    unsafe { s.unlock_current(&mut *a) };
    assert_eq!(prio(t[0]), 2, "M still waits on B, which L holds");
}

#[test]
fn timed_out_waiter_withdraws_its_boost() {
    let (mut s, _, t, _own) = sched_with(&[1, 3]);
    let mut m = mutex();
    assert_eq!(s.switch(), t[1]);
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[0]);
    unsafe { s.lock_current(&mut *m, None) };
    s.tick();
    assert_eq!(s.switch(), t[1]);
    unsafe { s.lock_current(&mut *m, Some(ms(2))) };
    assert_eq!(prio(t[0]), 3);
    s.switch();
    s.tick();
    s.tick(); // H times out
    assert_eq!(prio(t[0]), 1, "L kept the boost of a waiter that left");
    assert_eq!(m.owner(), t[0]);
}

#[test]
fn remaining_waiter_keeps_its_boost_when_another_times_out() {
    // L holds the mutex; H (timed) and M (untimed) wait. H gives up: L drops to 2, not 1.
    let (mut s, _, t, _own) = sched_with(&[1, 2, 3]);
    let mut m = mutex();
    assert_eq!(s.switch(), t[2]);
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[1]);
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[0]);
    unsafe { s.lock_current(&mut *m, None) };
    s.tick(); // H and M wake
    assert_eq!(s.switch(), t[2]);
    unsafe { s.lock_current(&mut *m, Some(ms(2))) }; // gives up at tick 3
    assert_eq!(s.switch(), t[0], "boosted L outranks M");
    s.sleep_current(at(2)); // L steps aside so M can block too
    assert_eq!(s.switch(), t[1]);
    unsafe { s.lock_current(&mut *m, None) };
    assert_eq!(prio(t[0]), 3);
    s.switch();
    s.tick(); // L wakes
    s.tick(); // H times out
    assert_eq!(prio(t[0]), 2, "M still waits on the mutex L holds");
    assert_eq!(m.owner(), t[0]);
}

// ---- M5: syscalls and task kill ----

#[test]
fn syscall_timeouts_round_up_and_survive_the_round_trip() {
    use crate::sched::{ticks_to_timeout, timeout_to_ticks};
    assert_eq!(timeout_to_ticks(None), u64::MAX, "forever");
    assert_eq!(
        timeout_to_ticks(Some(Duration::ZERO)),
        0,
        "try: never blocks"
    );
    assert_eq!(
        timeout_to_ticks(Some(Duration::from_micros(1))),
        1,
        "a wait must never end early"
    );
    assert_eq!(timeout_to_ticks(Some(ms(5))), 5);
    assert_ne!(
        timeout_to_ticks(Some(Duration::MAX)),
        u64::MAX,
        "a huge timeout is not forever"
    );
    for t in [None, Some(Duration::ZERO), Some(ms(1)), Some(ms(5))] {
        assert_eq!(ticks_to_timeout(timeout_to_ticks(t)), t);
    }
}

#[test]
fn killed_task_never_runs_again() {
    let (mut s, _, t, _own) = sched_with(&[1, 1]);
    assert_eq!(s.switch(), t[0]);
    s.kill_current();
    assert_eq!(s.switch(), t[1]);
    assert_eq!(s.switch(), t[1], "dead task was queued again");
    s.tick();
    assert_eq!(s.switch(), t[1]);
}

#[test]
fn killed_owner_leaves_its_mutex_locked() {
    let (mut s, _, t, _own) = sched_with(&[1, 2]);
    let mut m = mutex();
    assert_eq!(s.switch(), t[1]);
    s.sleep_current(at(1));
    assert_eq!(s.switch(), t[0]);
    unsafe { s.lock_current(&mut *m, None) };
    s.kill_current();
    s.switch();
    s.tick(); // the waiter wakes and tries the dead task's mutex
    assert_eq!(s.switch(), t[1]);
    assert_eq!(
        unsafe { s.lock_current(&mut *m, Some(ms(2))) },
        Lock::Blocked
    );
    assert_eq!(prio(t[0]), 2, "boosting a dead owner is harmless");
    s.switch();
    s.tick();
    s.tick();
    assert!(
        unsafe { (*t[1]).timed_out },
        "lock on a dead task's mutex must time out"
    );
    assert_eq!(m.owner(), t[0], "mutex stays locked");
}

// ---- M7: two cores ----

/// Like `sched_with`, plus an idle task for core 1. Returns
/// (sched, [idle0, idle1], tasks, owner).
fn smp_with(prios: &[u8]) -> (Sched, [*mut Task; 2], Vec<*mut Task>, Tasks) {
    let (mut s, idle0, t, mut own) = sched_with(prios);
    let idle1 = Box::into_raw(Box::new(Task::for_test(0)));
    unsafe { s.set_idle(1, idle1) };
    own.0.push(idle1);
    (s, [idle0, idle1], t, own)
}

/// Runs `switch` on `core`.
fn switch_on(s: &mut Sched, core: usize) -> *mut Task {
    s.set_core(core);
    s.switch()
}

#[test]
fn two_cores_run_the_two_most_urgent_tasks() {
    let (mut s, _, t, _own) = smp_with(&[1, 3, 2]);
    assert_eq!(switch_on(&mut s, 0), t[1]);
    assert_eq!(switch_on(&mut s, 1), t[2], "core 1 gets the next most urgent, not the same task");
}

#[test]
fn a_task_never_runs_on_both_cores() {
    let (mut s, idle, t, _own) = smp_with(&[1]);
    assert_eq!(switch_on(&mut s, 0), t[0]);
    assert_eq!(switch_on(&mut s, 1), idle[1], "core 1 took the task core 0 is running");
    // Core 0 time-slices: the task goes back on the ready list but core 0 is
    // still in the middle of its own switch; core 1 still must not take it.
    s.set_core(0);
    assert!(!s.should_switch_on(1), "nothing runnable for core 1");
}

#[test]
fn task_woken_before_its_switch_out_is_not_picked_by_the_other_core() {
    // The classic SMP race: core 0's task sleeps (PendSV not run yet, its
    // registers not saved), core 1 wakes it and switches. Core 1 must not run
    // it from a stale stack pointer.
    let (mut s, idle, t, _own) = smp_with(&[2, 1]);
    assert_eq!(switch_on(&mut s, 0), t[0]);
    assert_eq!(switch_on(&mut s, 1), t[1]);
    s.set_core(0);
    s.sleep_current(at(1));
    s.set_core(1);
    s.tick(); // t[0] is Ready again, but still on core 0
    assert!(!s.should_switch_on(1), "core 1 would pick a task still on core 0");
    assert_eq!(switch_on(&mut s, 1), t[1], "core 1 keeps its own task");
    // Core 0's PendSV saves t[0] and may now pick it itself.
    assert_eq!(switch_on(&mut s, 0), t[0]);
    let _ = idle;
}

#[test]
fn switched_out_task_becomes_runnable_for_the_other_core() {
    let (mut s, idle, t, _own) = smp_with(&[2, 3]);
    assert_eq!(switch_on(&mut s, 0), t[1]);
    assert_eq!(switch_on(&mut s, 1), t[0]);
    s.set_core(1);
    s.sleep_current(at(1));
    assert_eq!(switch_on(&mut s, 1), idle[1]);
    s.set_core(0);
    s.tick(); // t[0] wakes; nobody runs it; core 1 is idle
    assert!(s.should_switch_on(1), "the idle core must be told to pick it up");
    assert_eq!(switch_on(&mut s, 1), t[0]);
}

#[test]
fn waking_a_task_more_urgent_than_the_other_cores_task_targets_that_core() {
    let (mut s, _, t, _own) = smp_with(&[4, 1, 3]);
    let mut wl = wait_list();
    assert_eq!(switch_on(&mut s, 0), t[0]); // prio 4
    assert_eq!(switch_on(&mut s, 1), t[2]); // prio 3
    s.set_core(1);
    unsafe { s.block_current(&mut *wl, None) }; // the prio-3 task waits
    assert_eq!(switch_on(&mut s, 1), t[1]); // core 1 now runs prio 1
    s.set_core(0);
    unsafe { s.wake_one(&mut *wl) }; // core 0 (prio 4) gives
    assert!(!s.should_switch(), "core 0's prio-4 task stays");
    assert!(s.should_switch_on(1), "core 1 must preempt its prio-1 task");
}

#[test]
fn each_core_falls_back_to_its_own_idle() {
    let (mut s, idle, _, _own) = smp_with(&[]);
    assert_eq!(switch_on(&mut s, 0), idle[0]);
    assert_eq!(switch_on(&mut s, 1), idle[1]);
    assert_eq!(switch_on(&mut s, 0), idle[0], "idle never moves to the other core");
}

#[test]
fn equal_priority_does_not_preempt_the_other_core() {
    let (mut s, _, t, _own) = smp_with(&[1, 1, 1]);
    assert_eq!(switch_on(&mut s, 0), t[0]);
    assert_eq!(switch_on(&mut s, 1), t[1]);
    assert!(s.should_switch_on(1), "tick: core 1's time slice is up");
    assert!(!s.should_preempt(1), "a peer of equal priority must wait for the tick, or the cores ring each other forever");
}
