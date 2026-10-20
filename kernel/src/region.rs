//! MPU region encoding (ARMv8-M PMSA). Pure logic, host-tested.
//!
//! - RBAR: base[31:5] | SH[4:3] | AP[2:1] | XN[0]
//! - RLAR: limit[31:5] | AttrIndx[3:1] | EN[0]; `limit` is the *last* 32-byte
//!   block of the region (inclusive).

/// Peripheral regions a task can be granted (MPU regions 3..=7).
pub(crate) const PERIPHERAL_REGIONS: usize = 5;

/// RBAR.AP = read/write at any privilege level.
const AP_RW_ANY: u32 = 0b01 << 1;
/// RBAR.AP = read-only at any privilege level.
const AP_RO_ANY: u32 = 0b11 << 1;
/// RBAR.XN: execute never.
const XN: u32 = 1;
/// RLAR.AttrIndx 0: MAIR0 byte 0, normal memory (RAM, flash).
const ATTR_NORMAL: u32 = 0 << 1;
/// RLAR.AttrIndx 1: MAIR0 byte 1, device memory (peripheral registers).
const ATTR_DEVICE: u32 = 1 << 1;
/// RLAR.EN: region enabled.
const EN: u32 = 1;

/// MAIR0 matching the indices above: byte 0 = normal write-back (0xFF),
/// byte 1 = Device-nGnRE (0x04).
pub(crate) const MAIR0: u32 = 0x04 << 8 | 0xFF;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Region {
    pub(crate) rbar: u32,
    pub(crate) rlar: u32,
}

impl Region {
    fn encode(base: usize, len: usize, access: u32, attr: u32) -> Self {
        assert!(base % 32 == 0, "base needs to be 32-byte aligned");
        assert!(len % 32 == 0, "len needs to be 32-byte aligned");
        assert!(len > 0, "len has to be positive");
        Self {
            rbar: base as u32 | access,
            rlar: (base + len - 32) as u32 | attr | EN,
        }
    }

    pub(crate) fn code(base: usize, len: usize) -> Self {
        Self::encode(base, len, AP_RO_ANY, ATTR_NORMAL)
    }
    pub(crate) fn data(base: usize, len: usize) -> Self {
        Self::encode(base, len, AP_RW_ANY | XN, ATTR_NORMAL)
    }
    pub(crate) fn device(base: usize, len: usize) -> Self {
        Self::encode(base, len, AP_RW_ANY | XN, ATTR_DEVICE)
    }
    pub(crate) const DISABLED: Region = Region { rbar: 0, rlar: 0 };
}

#[cfg(test)]
mod tests;
