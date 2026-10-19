//! Host tests for MPU region encoding. Run: `cargo ktest`.
use super::*;

#[test]
fn limit_is_the_last_32_byte_block_inclusive() {
    let r = Region::data(0x2000_0000, 0x400);
    assert_eq!(r.rbar & !0x1F, 0x2000_0000);
    assert_eq!(r.rlar & !0x1F, 0x2000_03E0, "a 1 KiB region ends at block 0x3E0, not 0x400");
    let one = Region::data(0x2000_0040, 32);
    assert_eq!(one.rlar & !0x1F, 0x2000_0040, "a single block: limit == base");
}

#[test]
fn access_rules_per_kind() {
    let code = Region::code(0x1000_0000, 0x1000);
    assert_eq!(code.rbar & 0x7, 0b110, "flash: read-only for everyone, executable");
    let data = Region::data(0x2000_0000, 0x100);
    assert_eq!(data.rbar & 0x7, 0b011, "RAM: read/write for everyone, execute-never");
    let dev = Region::device(0xD000_0000, 0x100);
    assert_eq!(dev.rbar & 0x7, 0b011, "peripheral: read/write, execute-never");
    assert_eq!(code.rlar & 0xF, 0b0001, "normal memory (attr 0), enabled");
    assert_eq!(dev.rlar & 0xF, 0b0011, "device memory (attr 1), enabled");
    assert_eq!(MAIR0 & 0xFF, 0xFF, "attr 0: normal write-back");
    assert_eq!(MAIR0 >> 8 & 0xFF, 0x04, "attr 1: Device-nGnRE");
}

#[test]
fn disabled_region_is_not_enabled() {
    assert_eq!(Region::DISABLED.rlar & EN, 0);
}

#[test]
#[should_panic(expected = "32-byte aligned")]
fn unaligned_base_is_rejected() {
    Region::device(0xD000_0010, 0x100);
}

#[test]
#[should_panic(expected = "32-byte aligned")]
fn unaligned_length_is_rejected() {
    Region::device(0xD000_0000, 0x110);
}
