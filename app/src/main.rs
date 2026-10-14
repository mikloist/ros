//! M0: external LED on GP15 blinks via busy-wait.
#![no_std]
#![no_main]

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::OutputPin;

#[app::entry]
fn main() -> ! {
    let mut b = app::init();
    let mut led = b.pins.gpio15.into_push_pull_output();
    loop {
        defmt::info!("led on");
        led.set_high().unwrap();
        b.timer.delay_ms(500);
        defmt::info!("led off");
        led.set_low().unwrap();
        b.timer.delay_ms(500);
    }
}
