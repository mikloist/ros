# M0: Workspace, HAL Boot, Test Harness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Embassy blinky with a `kernel` + `app` workspace that boots on `rp235x-hal`, blinks an external LED on GP15, and has on-board test binaries whose pass/fail shows up as the `probe-rs run` exit code.

**Architecture:** Cargo workspace. `kernel` is an empty `no_std` lib, host-testable via `cargo ktest`. `app` has a small lib (`init()`, `exit_ok()`, boot block) shared by `src/main.rs` and per-milestone binaries in `src/bin/`.

**Tech Stack:** Rust 2024, `thumbv8m.main-none-eabihf`, `rp235x-hal` 0.4, `cortex-m` 0.7, `cortex-m-rt` 0.7, `cortex-m-semihosting` 0.5, `defmt` 1 / `defmt-rtt` 1, `panic-probe` 1, `probe-rs` 0.32.

**Spec:** `docs/superpowers/specs/2026-10-14-rtos-kernel-design.md` (section 1 and 6, milestone M0)

## Global Constraints

- Target `thumbv8m.main-none-eabihf`, chip `RP235x`, Secure Arm image (`ImageDef::secure_exe()`).
- No Embassy, `cyw43`, or `static_cell` dependencies.
- `critical-section` impl comes from `cortex-m` feature `critical-section-single-core`; do NOT enable `rp235x-hal`'s `critical-section-impl` (two impls fail to link).
- Do NOT enable `rp235x-hal`'s `defmt` feature (it pulls defmt 0.3).
- Edition 2024: link sections are written `#[unsafe(link_section = "...")]`.
- Verified facts (checked on this board 2026-10-14, throwaway build):
  - `cortex_m::asm::bkpt()` makes `probe-rs run` exit 1 (`Breakpoint(Unknown)`); passing tests MUST exit via `cortex_m_semihosting::debug::exit(EXIT_SUCCESS)` → exit 0.
  - A panic through `panic-probe` → exit 1 with message and backtrace.
  - `probe-rs run` attach is sometimes slow (observed 1–45 s before firmware starts). Use timeouts ≥ 120 s for board runs; a short timeout looks like a silent hang.
  - DEMCR.VC_HARDERR is already set at the first instruction, so no startup delay is needed for fault catching.

---

### Task 1: Workspace, HAL boot, GP15 blinky

**Files:**
- Create: `Cargo.toml` (replace), `.cargo/config.toml` (replace), `kernel/Cargo.toml`, `kernel/src/lib.rs`, `app/Cargo.toml`, `app/build.rs`, `app/src/lib.rs`, `app/src/main.rs`
- Move: `memory.x` → `app/memory.x`, `cyw43-firmware/` → `app/cyw43-firmware/`
- Delete: `src/main.rs`, `build.rs`, `Cargo.lock` (regenerated)

**Interfaces:**
- Produces (used by every later milestone binary):
  - `app::entry` (re-export of `rp235x_hal::entry`)
  - `app::init() -> app::Board`, `pub struct Board { pub pins: rp235x_hal::gpio::Pins, pub timer: rp235x_hal::Timer<rp235x_hal::timer::CopyableTimer0> }`
  - `app::exit_ok() -> !`
  - `cargo ktest` alias = host tests for `kernel`

- [ ] **Step 1: Commit the existing Embassy blinky as a baseline**

```bash
git add -A
git commit -m "chore: baseline Embassy blinky before RTOS restructure"
```

- [ ] **Step 2: Move and delete files**

```bash
mkdir -p app/src/bin kernel/src
git mv memory.x app/memory.x
git mv cyw43-firmware app/cyw43-firmware
git rm -q src/main.rs build.rs
rm Cargo.lock
```

- [ ] **Step 3: Write workspace `Cargo.toml`**

```toml
[workspace]
resolver = "3"
members = ["kernel", "app"]

[profile.release]
debug = true
```

- [ ] **Step 4: Write `.cargo/config.toml`**

```toml
[target.'cfg(all(target_arch = "arm", target_os = "none"))']
runner = "probe-rs run --chip RP235x"

[build]
target = "thumbv8m.main-none-eabihf"

[alias]
ktest = "test -p kernel --target x86_64-unknown-linux-gnu"

[env]
DEFMT_LOG = "debug"
```

- [ ] **Step 5: Write `kernel/Cargo.toml` and `kernel/src/lib.rs`**

```toml
[package]
name = "kernel"
version = "0.1.0"
edition = "2024"

[dependencies]
```

```rust
//! ros kernel. Pure logic is host-testable; `arch` is Cortex-M33 only.
#![cfg_attr(not(test), no_std)]
```

- [ ] **Step 6: Write `app/Cargo.toml`**

```toml
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
kernel = { path = "../kernel" }
rp235x-hal = { version = "0.4", features = ["rt", "binary-info"] }
cortex-m = { version = "0.7", features = ["critical-section-single-core"] }
cortex-m-rt = "0.7"
cortex-m-semihosting = "0.5"
embedded-hal = "1"
defmt = "1"
defmt-rtt = "1"
panic-probe = { version = "1", features = ["print-defmt"] }
```

- [ ] **Step 7: Write `app/build.rs`**

Copies `memory.x` into `OUT_DIR` so the linker finds it whatever the working directory.

```rust
use std::{env, fs, path::PathBuf};

fn main() {
    // Put memory.x on the linker search path (cwd-independent in a workspace).
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy("memory.x", out.join("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
}
```

- [ ] **Step 8: Write `app/src/lib.rs`**

```rust
//! Board support shared by every demo binary.
#![no_std]

use rp235x_hal as hal;
use {defmt_rtt as _, panic_probe as _};

pub use hal::entry;

/// Tells the RP2350 boot ROM this is a Secure Arm executable.
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

/// Pico 2 W crystal.
pub const XTAL_FREQ_HZ: u32 = 12_000_000;

pub struct Board {
    pub pins: hal::gpio::Pins,
    pub timer: hal::Timer<hal::timer::CopyableTimer0>,
}

/// Clocks to 150 MHz, GPIO bank, TIMER0.
pub fn init() -> Board {
    let mut pac = hal::pac::Peripherals::take().unwrap();
    let mut watchdog = hal::Watchdog::new(pac.WATCHDOG);
    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_FREQ_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .unwrap();
    let timer = hal::Timer::new_timer0(pac.TIMER0, &mut pac.RESETS, &clocks);
    let sio = hal::Sio::new(pac.SIO);
    let pins = hal::gpio::Pins::new(pac.IO_BANK0, pac.PADS_BANK0, sio.gpio_bank0, &mut pac.RESETS);
    Board { pins, timer }
}

/// End a test binary successfully: `probe-rs run` exits 0.
pub fn exit_ok() -> ! {
    defmt::info!("TEST PASS");
    cortex_m_semihosting::debug::exit(cortex_m_semihosting::debug::EXIT_SUCCESS);
    unreachable!()
}
```

- [ ] **Step 9: Write `app/src/main.rs`**

```rust
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
```

- [ ] **Step 10: Build and host-test**

Run: `cargo build --release && cargo ktest`
Expected: `Finished`, then `test result: ok. 0 passed` (no kernel tests yet; this proves the host harness compiles the crate).

- [ ] **Step 11: Wire LED and run on the board**

Wire: GP15 (pin 20) → 330 Ω resistor → LED anode; LED cathode → GND (pin 18).

Run: `timeout -s INT 120 cargo run --release --bin app`
Expected: alternating `led on` / `led off` RTT lines every 500 ms and the LED blinking. Ctrl-C to stop.

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "feat(m0): kernel/app workspace on rp235x-hal, GP15 blinky"
```

---

### Task 2: Pass/fail test harness binaries

**Files:**
- Create: `app/src/bin/m0_pass.rs`, `app/src/bin/m0_fail.rs`

**Interfaces:**
- Consumes: `app::entry`, `app::init()`, `app::exit_ok()` from Task 1.
- Produces: the on-board test convention every later milestone uses: a binary in `app/src/bin/mN_*.rs` passes iff `cargo run --release --bin mN_*` exits 0.

- [ ] **Step 1: Write `app/src/bin/m0_pass.rs`**

```rust
//! M0: proves a passing test binary makes `probe-rs run` exit 0.
#![no_std]
#![no_main]

#[app::entry]
fn main() -> ! {
    app::init();
    app::exit_ok()
}
```

- [ ] **Step 2: Write `app/src/bin/m0_fail.rs`**

```rust
//! M0: proves a failing assert makes `probe-rs run` exit non-zero.
#![no_std]
#![no_main]

#[app::entry]
fn main() -> ! {
    app::init();
    assert_eq!(1 + 1, 3, "deliberate failure");
    app::exit_ok()
}
```

- [ ] **Step 3: Run the pass binary**

Run: `timeout -s INT 120 cargo run --release --bin m0_pass; echo EXIT=$?`
Expected: `TEST PASS`, `Firmware exited successfully`, `EXIT=0`.

- [ ] **Step 4: Run the fail binary**

Run: `timeout -s INT 120 cargo run --release --bin m0_fail; echo EXIT=$?`
Expected: `panicked at app/src/bin/m0_fail.rs`, `assertion ... deliberate failure`, `HardFault <Cause: Escalated UsageFault (Undefined instruction)>`, `EXIT=1`.
`EXIT=124` means the timeout hit during the slow probe-rs attach, not a test result; rerun.

- [ ] **Step 5: Commit**

```bash
git add app/src/bin/m0_pass.rs app/src/bin/m0_fail.rs
git commit -m "test(m0): on-board pass/fail harness via semihosting exit"
```
