//! Host tests for the ring buffer. Run: `cargo ktest`.
use super::*;
use std::rc::Rc;

#[test]
fn fifo_order() {
    let mut r: Ring<u32, 4> = Ring::new();
    for i in 1..=3 {
        r.push(i).unwrap();
    }
    assert_eq!(
        [r.pop(), r.pop(), r.pop(), r.pop()],
        [Some(1), Some(2), Some(3), None]
    );
}

#[test]
fn full_ring_rejects_and_returns_item() {
    let mut r: Ring<u32, 2> = Ring::new();
    r.push(1).unwrap();
    r.push(2).unwrap();
    assert!(r.is_full());
    assert_eq!(r.push(3), Err(3));
}

#[test]
fn wraps_around_many_times() {
    let mut r: Ring<u32, 3> = Ring::new();
    let mut next_out = 0;
    for i in 0..100 {
        r.push(i).unwrap();
        if r.is_full() {
            assert_eq!(r.pop(), Some(next_out));
            next_out += 1;
        }
    }
    while let Some(v) = r.pop() {
        assert_eq!(v, next_out);
        next_out += 1;
    }
    assert_eq!(next_out, 100);
}

#[test]
fn dropping_ring_drops_queued_items() {
    let item = Rc::new(());
    {
        let mut r: Ring<Rc<()>, 4> = Ring::new();
        r.push(item.clone()).unwrap();
        r.push(item.clone()).unwrap();
        r.pop();
        assert_eq!(Rc::strong_count(&item), 2);
    }
    assert_eq!(Rc::strong_count(&item), 1, "queued item leaked");
}
