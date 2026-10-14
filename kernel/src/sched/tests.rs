//! Host tests for the scheduler. Run: `cargo ktest`.
use super::*;

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
        s.make_ready(idle);
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
    assert!(s.sleep_current(5));
    assert_eq!(s.switch(), idle);
}

#[test]
fn sleep_until_now_or_past_does_not_sleep() {
    let (mut s, _, t, _own) = sched_with(&[1]);
    s.switch();
    s.tick();
    s.tick();
    assert!(!s.sleep_current(2), "deadline == now");
    assert!(!s.sleep_current(1), "deadline in the past");
    assert_eq!(s.switch(), t[0], "task should still be runnable");
}

#[test]
fn sleeper_wakes_on_its_tick_and_not_before() {
    let (mut s, idle, t, _own) = sched_with(&[1]);
    s.switch();
    s.sleep_current(3);
    assert_eq!(s.switch(), idle);
    assert!(!s.tick(), "tick 1: nothing woke");
    assert!(!s.tick(), "tick 2: nothing woke");
    assert!(s.tick(), "tick 3: task woke and outranks idle");
    assert_eq!(s.now(), 3);
    assert_eq!(s.switch(), t[0]);
}

#[test]
fn sleepers_wake_by_deadline_then_fifo_on_ties() {
    let (mut s, idle, t, _own) = sched_with(&[1, 1, 1]);
    assert_eq!(s.switch(), t[0]);
    s.sleep_current(5);
    assert_eq!(s.switch(), t[1]);
    s.sleep_current(3);
    assert_eq!(s.switch(), t[2]);
    s.sleep_current(5);
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
    s.sleep_current(2);
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
    s.sleep_current(1);
    s.tick(); // wakes t[0] while it is still `current`
    assert_eq!(s.switch(), t[0]);
    s.sleep_current(10);
    assert_eq!(s.switch(), idle, "a stale second queue entry ran a sleeping task");
}

#[test]
fn tick_before_start_requests_nothing() {
    let (mut s, _, _, _own) = sched_with(&[1]);
    assert!(!s.tick());
}
