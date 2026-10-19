//! M3: queue. A producer (prio 1) streams 200 numbers to a consumer (prio 2)
//! through a 4-slot queue. The consumer pauses now and then so the queue fills
//! and the producer must block on "full". Then: full queue rejects try_send,
//! send times out and returns the item, empty queue times out recv.
#![no_std]
#![no_main]

use kernel::{Duration, Queue, Semaphore, Stack, Task, Timeout};

const ITEMS: u32 = 200;
static Q: Queue<u32, 4> = Queue::new();
/// Producer -> consumer: "the queue is full and the timeouts are checked".
static FILLED: Semaphore = Semaphore::new(0);
static STACK_P: Stack<1024> = Stack::new();
static STACK_C: Stack<1024> = Stack::new();

fn producer() -> ! {
    for i in 0..ITEMS {
        Q.send(i, None).unwrap();
    }
    // The consumer is now asleep: fill the queue and check "full" behavior.
    for i in 0..4 {
        Q.try_send(1000 + i).unwrap();
    }
    assert_eq!(Q.try_send(7), Err(7), "try_send on a full queue");
    let t0 = kernel::now();
    assert_eq!(
        Q.send(8, Some(Duration::from_millis(3))),
        Err(8),
        "timed send on a full queue"
    );
    assert_eq!(
        kernel::now() - t0,
        Duration::from_millis(3),
        "timed send did not last 3 ticks"
    );
    defmt::info!("full queue: try_send rejected, send timed out and returned the item");
    FILLED.give();
    loop {
        kernel::sleep(Duration::from_secs(1));
    }
}

fn consumer() -> ! {
    for i in 0..ITEMS {
        assert_eq!(Q.recv(None), Ok(i), "items out of order");
        if i % 50 == 49 {
            kernel::sleep(Duration::from_millis(1)); // let the producer fill the queue and block
        }
    }
    defmt::info!("{} items in order", ITEMS);
    FILLED.take(None).unwrap();
    for i in 0..4 {
        assert_eq!(Q.try_recv(), Ok(1000 + i));
    }
    let t0 = kernel::now();
    assert_eq!(
        Q.recv(Some(Duration::from_millis(3))),
        Err(Timeout),
        "recv on an empty queue"
    );
    assert_eq!(
        kernel::now() - t0,
        Duration::from_millis(3),
        "timed recv did not last 3 ticks"
    );
    defmt::info!("empty queue: recv timed out");
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let b = app::init();
    let mut tasks = [
        Task::new("producer", 1, producer, &STACK_P),
        Task::new("consumer", 2, consumer, &STACK_C),
    ];
    kernel::start(&mut tasks, b.sys_hz)
}
