#![no_std]

use drv_spi_api::SpiError;

#[derive(Copy, Clone, PartialEq)]
pub enum VscError {
    SpiError(SpiError),
    BadChipId(u32),
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

    /// Mismatch in the `IDENTIFIER_1` PHY register
    BadId1(u16),
    /// Mismatch in the `IDENTIFIER_2` PHY register
    BadId2(u16),
    /// Indicates that the VSC8504 is not Tesla E silicon
    BadRev,
    InitTimeout,

    MiimReadErr {
        miim: u32,
        phy: u8,
        page: u16,
        addr: u8,
    },
    MiimIdleTimeout,
    MiimReadTimeout,
}

impl From<SpiError> for VscError {
    fn from(s: SpiError) -> Self {
        Self::SpiError(s)
    }
}
