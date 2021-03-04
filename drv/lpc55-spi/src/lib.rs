//! A driver for the LPC55 HighSpeed SPI interface.
//!
//! Mostly for demonstration purposes, write is verified read is not
//!
//! # IPC protocol
//!
//! ## `write` (1)
//!
//! Sends the contents of lease #0. Returns when completed.
//!
//!
//! ## `read` (2)
//!
//! Reads the buffer into lease #0. Returns when completed

#![no_std]

use lpc55_pac as device;

pub struct Spi {
    reg: &'static device::spi0::RegisterBlock,
}

impl From<&'static device::spi0::RegisterBlock> for Spi {
    fn from(reg: &'static device::spi0::RegisterBlock) -> Self {
        Self { reg }
    }
}

#[repr(u32)]
pub enum TxLvl {
    TxEmpty = 0,
    Tx1Item = 1,
    Tx2Items = 2,
    Tx3Items = 3,
    Tx4Items = 4,
    Tx5Items = 5,
    Tx6Items = 6,
    Tx7Items = 7,
}

impl TxLvl {
    fn to_bits(&self) -> u8 {
        match self {
            TxLvl::TxEmpty => 0,
            TxLvl::Tx1Item => 1,
            TxLvl::Tx2Items => 2,
            TxLvl::Tx3Items => 3,
            TxLvl::Tx4Items => 4,
            TxLvl::Tx5Items => 5,
            TxLvl::Tx6Items => 6,
            TxLvl::Tx7Items => 7,
        }
    }
}

#[repr(u32)]
pub enum RxLvl {
    Rx1Item = 0,
    Rx2Items = 1,
    Rx3Items = 2,
    Rx4Items = 3,
    Rx5Items = 4,
    Rx6Items = 5,
    Rx7Items = 6,
    Rx8Items = 7,
}

impl RxLvl {
    fn to_bits(&self) -> u8 {
        match self {
            RxLvl::Rx1Item => 0,
            RxLvl::Rx2Items => 1,
            RxLvl::Rx3Items => 2,
            RxLvl::Rx4Items => 3,
            RxLvl::Rx5Items => 4,
            RxLvl::Rx6Items => 5,
            RxLvl::Rx7Items => 6,
            RxLvl::Rx8Items => 7,
        }
    }
}

impl Spi {
    pub fn initialize(
        &mut self,
        master: device::spi0::cfg::MASTER_A,
        lsbf: device::spi0::cfg::LSBF_A,
        cpha: device::spi0::cfg::CPHA_A,
        cpol: device::spi0::cfg::CPOL_A,
        tx_lvl: TxLvl,
        rx_lvl: RxLvl,
    ) {
        // Ensure the block is off
        self.reg
            .fifocfg
            .modify(|_, w| w.enabletx().disabled().enablerx().disabled());

        self.reg.cfg.modify(|_, w| {
            w.enable()
                // Keep this off while we're configuring
                .disabled()
                .master()
                .variant(master)
                .lsbf()
                .variant(lsbf)
                .cpha()
                .variant(cpha)
                .cpol()
                .variant(cpol)
                // Loopback feature for testing, always keep off for now
                .loop_()
                .disabled()
        });

        // Just trigger the FIFOs to hold 1 item for now
        self.reg.fifotrig.modify(|_, w| unsafe {
            w.txlvlena()
                .enabled()
                .txlvl()
                .bits(tx_lvl.to_bits())
                .rxlvlena()
                .enabled()
                .rxlvl()
                .bits(rx_lvl.to_bits())
        });
    }

    pub fn enable(&mut self) {
        self.reg
            .fifocfg
            .modify(|_, w| w.enabletx().enabled().enablerx().enabled());

        self.reg.cfg.modify(|_, w| w.enable().enabled());
    }

    pub fn can_tx(&self) -> bool {
        self.reg.fifostat.read().txnotfull().bit_is_set()
    }

    pub fn has_byte(&self) -> bool {
        self.reg.fifostat.read().rxnotempty().bit_is_set()
    }

    pub fn enable_tx(&mut self) {
        self.reg.fifointenset.write(|w| w.txlvl().enabled());
    }

    pub fn enable_rx(&mut self) {
        self.reg.fifointenset.write(|w| w.rxlvl().enabled());
    }

    pub fn disable_tx(&mut self) {
        self.reg.fifointenclr.write(|w| w.txlvl().set_bit());
    }

    pub fn disable_rx(&mut self) {
        self.reg.fifointenclr.write(|w| w.rxlvl().set_bit());
    }

    pub fn send_u8(&mut self, byte: u8) {
        self.reg.fifowr.write(|w| unsafe {
            w.len()
                // Hard code number of bits sent
                .bits(7)
                // Don't wait for RX while we're TX (may need to change)
                .rxignore()
                .read()
                .txdata()
                .bits(byte as u16)
        });
    }

    pub fn get_fifostat(&self) -> u32 {
        self.reg.fifointstat.read().bits()
    }

    pub fn read_u8(&mut self) -> u8 {
        // TODO SOT flag?
        self.reg.fiford.read().rxdata().bits() as u8
    }
}
