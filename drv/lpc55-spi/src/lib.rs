// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

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

    pub fn drain(&mut self) {
        self.reg
            .fifocfg
            .modify(|_, w| w.emptytx().set_bit().emptyrx().set_bit());
    }

    pub fn drain_tx(&mut self) {
        self.reg.fifocfg.modify(|_, w| w.emptytx().set_bit());
    }

    pub fn enable(&mut self) {
        self.drain();
        self.reg
            .fifocfg
            .modify(|_, w| w.enabletx().enabled().enablerx().enabled());

        self.reg.cfg.modify(|_, w| w.enable().enabled());
    }

    pub fn mstidle(&self) -> bool {
        self.reg.stat.read().mstidle().bit_is_set()
    }

    // This should really be upstreamed into the lpc55-pac crate For some
    // reason the SSD and SSA flags are not supported as readable However,
    // this is useful in polling mode when we don't want to rely on interrupts
    // necessarily, or don't want to worry about the flags automatically being
    // cleared in `intstat` if we only care about one interrupt type.
    pub fn ssd(&self) -> bool {
        (self.reg.stat.read().bits() >> 5) & 0x01 != 0
    }

    pub fn can_tx(&self) -> bool {
        self.reg.fifostat.read().txnotfull().bit_is_set()
    }

    pub fn has_entry(&self) -> bool {
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

    pub fn ssa_enable(&mut self) {
        self.reg.intenset.write(|w| w.ssaen().set_bit());
    }

    pub fn ssa_disable(&mut self) {
        self.reg.intenclr.write(|w| w.ssaen().set_bit());
    }

    /// Clear Slave Select Asserted interrupt
    pub fn ssa_clear(&self) {
        self.reg.stat.write(|w| w.ssa().set_bit());
    }

    pub fn ssd_enable(&mut self) {
        self.reg.intenset.write(|w| w.ssden().set_bit());
    }

    pub fn ssd_disable(&mut self) {
        self.reg.intenclr.write(|w| w.ssden().set_bit());
    }

    /// Clear Slave Select De-asserted interrupt
    pub fn ssd_clear(&mut self) {
        self.reg.stat.write(|w| w.ssd().set_bit());
    }

    pub fn mstidle_enable(&mut self) {
        self.reg.intenset.write(|w| w.mstidleen().set_bit());
    }

    pub fn mstidle_disable(&mut self) {
        self.reg.intenclr.write(|w| w.mstidle().set_bit());
    }

    pub fn send_u8_no_rx(&mut self, byte: u8) {
        self.send_raw_data(byte as u16, true, true, 8)
    }

    pub fn send_u8(&mut self, byte: u8) {
        self.reg.fifowr.write(|w| unsafe {
            w.len()
                // Data length, per NXP docs:
                //
                // 0x0-2 = Reserved.
                // 0x3 = Data transfer is 4 bits in length.
                // 0x4 = Data transfer is 5 bits in length.
                // ...
                // 0xF = Data transfer is 16 bits in length.
                .bits(7)
                .txdata()
                .bits(byte as u16)
        });
    }

    /// This part is really weird.
    ///
    /// The TX and RX FIFOs operate in terms of entries. There are 8 16-bit
    /// entries, which is what txlvl/rxlvel in fifostat account for. However,
    /// the frame size (number of bits per fifo entry) sent and received does
    /// not have to be the full 16-bits per-entry. The frame size is configured
    /// on each `fifowr.write` by writing the upper half-word of the fifowr
    /// register, where that size can be anywhere from 4 to 16 bits. The frame
    /// size adjustment allows us to only use part of an entry, and operate in
    /// terms of bytes for simplicity. We do this with `send_u8` and `read_u8`.
    /// Importantly, when we call `send_u8` we are configuring not only the
    /// write fifo to use 8-bits per entry, but *also* the read fifo. Therefore
    /// we use `send_u8` and `read_u8` in a pair. We don't mix and match to
    /// prevent confusion and errors.
    ///
    /// We want to do the same thing if we use the full 16-bit fifos. We first
    /// configure the fifos by calling `send_u16`. From there on out we can
    /// just use `send_u16` and `read_u16` in pairs to double our throughput
    /// and take full advantage of the fifos.
    ///
    /// Note that we could just write the control bits of the FIFOWR register
    /// but that would leave us in a weird place where we wouldn't know what state
    /// the fifo was in. Since FIFOWR is write-only, we just set the frame size
    /// on each wite and ensure we pair writes and reads of the same width.
    pub fn send_u16(&self, entry: u16) {
        self.reg.fifowr.write(|w| unsafe {
            w.len()
                // 0xF = Data transfer is 16 bits in length.
                .bits(0xF)
                .txdata()
                .bits(entry)
        });
    }

    pub fn send_raw_data(
        &mut self,
        data: u16,
        eot: bool,
        rxignore: bool,
        len_bits: u8,
    ) {
        // SPI hardware only supports lengths of range 4-16 bits
        #[allow(clippy::manual_range_contains)]
        if len_bits > 16 || len_bits < 4 {
            panic!()
        }

        self.reg.fifowr.write(|w| unsafe {
            w.len()
                // Data length, per NXP docs:
                //
                // 0x0-2 = Reserved.
                // 0x3 = Data transfer is 4 bits in length.
                // 0x4 = Data transfer is 5 bits in length.
                // ...
                // 0xF = Data transfer is 16 bits in length.
                .bits(len_bits - 1)
                // Don't wait for RX while we're TX (may need to change)
                .rxignore()
                .bit(rxignore)
                // We need to make sure this gets deasserted so that we can
                // know when MST goes idle
                .eot()
                .bit(eot)
                .txdata()
                .bits(data)
        });
    }

    pub fn get_fifostat(&self) -> u32 {
        self.reg.fifointstat.read().bits()
    }

    /// Destructive read of SPI Interrupt Status Register.
    /// Slave select assert - set on transitions from de-asserted to asserted.
    /// Slave select de-assert - set on transitions from asserted to de-asserted.
    /// Master idle status flag - true when master function is fully idle.
    // N.B. Reading this register clears the interrupt conditions.
    // NXP Document UM11126 35.6.8 SPI interrupt status register
    pub fn intstat(&self) -> device::spi0::intstat::R {
        self.reg.intstat.read()
    }

    pub fn stat(&self) -> device::spi0::stat::R {
        self.reg.stat.read()
    }

    pub fn fifostat(&mut self) -> device::spi0::fifostat::R {
        self.reg.fifostat.read()
    }

    pub fn txerr_clear(&mut self) {
        self.reg.fifostat.modify(|_, w| w.txerr().set_bit());
    }

    pub fn rxerr_clear(&mut self) {
        self.reg.fifostat.modify(|_, w| w.rxerr().set_bit());
    }

    /// This should only be used with the corresponding `send_u8` method.
    /// Seriously: Ensure that the entry size in the fifo is actually 8 bits
    /// as configured in the FIFOWR register.
    ///
    /// Mixing and matching different frame sizes is not recommended.
    pub fn read_u8(&mut self) -> u8 {
        self.reg.fiford.read().rxdata().bits() as u8
    }

    /// This should only be used with the corresponding `send_u16` method.
    /// Seriously: Ensure that the entry size in the fifo is actually 16 bits
    /// as configured in the FIFOWR register.
    ///
    /// Mixing and matching different frame sizes is not recommended.
    pub fn read_u16(&mut self) -> u16 {
        self.reg.fiford.read().rxdata().bits() as u16
    }

    pub fn read_u16_with_sot(&self) -> (u16, bool) {
        let reader = self.reg.fiford.read();
        (reader.rxdata().bits() as u16, reader.sot().bit_is_set())
    }
}
