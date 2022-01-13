#![no_std]

pub mod dev;
pub mod phy;
pub mod port;
pub mod serdes10g;
pub mod serdes1g;
pub mod serdes6g;
pub mod spi;

use drv_spi_api::SpiError;

#[derive(Copy, Clone, PartialEq)]
pub enum VscError {
    SpiError(SpiError),
    BadChipId(u32),
    MiimReadErr {
        miim: u8,
        phy: u8,
        page: u16,
        addr: u8,
    },
    BadPhyId1(u16),
    BadPhyId2(u16),
    MiimIdleTimeout,
    MiimReadTimeout,
    Serdes6gReadTimeout {
        instance: u32,
    },
    Serdes6gWriteTimeout {
        instance: u32,
    },
    PortFlushTimeout {
        port: u32,
    },
    AnaCfgTimeout,
    SerdesFrequencyTooLow(u64),
    SerdesFrequencyTooHigh(u64),
    TriDecFailed(u16),
    BiDecFailed(u16),
    LtDecFailed(u16),
    LsDecFailed(u16),
    TxPllLockFailed,
    TxPllFsmFailed,
    RxPllLockFailed,
    RxPllFsmFailed,
    OffsetCalFailed,
}

impl From<SpiError> for VscError {
    fn from(s: SpiError) -> Self {
        Self::SpiError(s)
    }
}
