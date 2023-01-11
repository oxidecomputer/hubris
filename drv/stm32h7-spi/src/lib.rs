// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32H7 SPI, in host mode.
//!
//! This is the core logic, separated from the IPC server. The peripheral also
//! supports I2S, which we haven't bothered implementing because we don't have a
//! need for it.
//!
//! # Clocking
//!
//! The SPI block has no fewer than three clock domains.
//!
//! 1. `pclk` contains most of the control logic and operates at the APB
//!    frequency.
//!
//! 2. `ker_ck` contains the clock generator and is driven as a "kernel clock"
//!    from the RCC -- there is a separate mux there to choose its source.
//!
//! 3. The "serial interface domain" (no catchy abbreviation provided) is
//!    clocked at the external SCK rate. This is derived from `ker_ck` in host
//!    role.
//!
//! In host role, the SPI needs to have at least `ker_ck` running to do useful
//! work.
//!
//! # Automagic CRC generation
//!
//! We do not currently support the hardware's automatic CRC features.
//!
//! # Why is everything `spi1`
//!
//! The `stm32h7` PAC crate we currently use has decided that all SPI types
//! should be called `spi1`. This is despite significant hardware differences
//! between SPI1-6. Our is not to question why.

#![no_std]

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

#[derive(Clone)]
pub struct Spi {
    /// Pointer to our register block.
    ///
    /// This is not a `SPIx` type from the `stm32h7` crate because then we're
    /// generic for no good reason and type parameters multiply. Ew.
    reg: &'static device::spi1::RegisterBlock,
}

impl From<&'static device::spi1::RegisterBlock> for Spi {
    fn from(reg: &'static device::spi1::RegisterBlock) -> Self {
        Self { reg }
    }
}

impl Spi {
    pub fn initialize(
        &mut self,
        mbr: device::spi1::cfg1::MBR_A,
        bits_per_frame: u8,
        comm: device::spi1::cfg2::COMM_A,
        lsbfrst: device::spi1::cfg2::LSBFRST_A,
        cpha: device::spi1::cfg2::CPHA_A,
        cpol: device::spi1::cfg2::CPOL_A,
        ssom: device::spi1::cfg2::SSOM_A,
    ) {
        // Expected preconditions:
        // - GPIOs configured to proper AF etc - we cannot do this, because we
        // cannot presume to have either direct GPIO access _or_ IPC access.
        // - Clock on, reset off - again, we can't do this directly.

        assert!((4..=32).contains(&bits_per_frame));

        // Write CFG1/CFG2 to configure
        self.reg
            .cfg1
            .write(|w| w.mbr().variant(mbr).dsize().bits(bits_per_frame - 1));

        // TODO: C driver has some bits about twiddling SSI state to avoid MODF.
        // I've hardcoded what I believe is the equivalent result here.

        self.reg.cr1.write(|w| w.ssi().set_bit());

        #[rustfmt::skip]
        self.reg.cfg2.write(|w| {
            w
                // This bit determines if software manages SS (SSM = 1) or
                // hardware (SSM = 0). We are doing software.
                .ssm().set_bit()
                // SS output disabled.
                .ssoe().enabled()
                // Don't glitch pins when being reconfigured.
                .afcntr().controlled()
                // This is currently a host-only driver.
                .master().set_bit()
                .comm().variant(comm)
                .lsbfrst().variant(lsbfrst)
                .cpha().variant(cpha)
                .cpol().variant(cpol)
                .ssom().variant(ssom)
        });

        self.reg.i2scfgr.write(|w| w.i2smod().clear_bit());
    }

    pub fn enable(&self, tsize: u16, div: device::spi1::cfg1::MBR_A) {
        self.reg.cfg1.modify(|_, w| w.mbr().variant(div));
        self.reg.cr2.modify(|_, w| w.tsize().bits(tsize));
        self.reg.cr1.modify(|_, w| w.spe().set_bit());
    }

    pub fn start(&self) {
        self.reg.cr1.modify(|_, w| w.cstart().set_bit());
        // Clear EOT flag
        self.reg.ifcr.write(|w| w.eotc().set_bit());
    }

    pub fn can_rx_word(&self) -> bool {
        let sr = self.reg.sr.read();
        sr.rxwne().bit()
    }

    pub fn can_rx_byte(&self) -> bool {
        let sr = self.reg.sr.read();
        sr.rxwne().bit() || sr.rxplvl().bits() != 0
    }

    pub fn can_tx_frame(&self) -> bool {
        let sr = self.reg.sr.read();
        sr.txp().bit()
    }

    pub fn send32(&self, bytes: u32) {
        self.reg.txdr.write(|w| w.txdr().bits(bytes));
    }

    pub fn end_of_transmission(&self) -> bool {
        let sr = self.reg.sr.read();
        sr.eot().bit()
    }

    pub fn set_data_line_swap(&self, flag: bool) {
        self.reg.cfg2.modify(|_, w| w.ioswp().bit(flag));
    }

    /// Stuffs one byte of data into the SPI TX FIFO.
    ///
    /// Preconditions:
    ///
    /// - There must be room for a byte in the TX FIFO (call `can_tx_frame` to
    ///   check, or call this in response to a TXP interrupt).
    pub fn send8(&self, byte: u8) {
        // The TXDR register can be accessed as a byte, halfword, or word. This
        // determines how many bytes are pushed in. stm32h7/svd2rust don't
        // understand this, and so we have to get a pointer to the byte portion
        // of the register manually and dereference it.

        // Because svd2rust didn't see this one coming, we cannot get a direct
        // reference to the VolatileCell within the wrapped Reg type of txdr,
        // nor will the Reg type agree to give us a pointer to its contents like
        // VolatileCell will, presumably to save us from ourselves. And thus we
        // must exploit the fact that VolatileCell is the only (non-zero-sized)
        // member of Reg, and in fact _must_ be for Reg to work correctly when
        // used to overlay registers in memory.

        // Safety: "Downcast" txdr to a pointer to its sole member, whose type
        // we know because of our unholy source-code-reading powers.
        let txdr: &vcell::VolatileCell<u32> =
            unsafe { core::mem::transmute(&self.reg.txdr) };
        // vcell is more pleasant and will happily give us the pointer we want.
        let txdr: *mut u32 = txdr.as_ptr();
        // As we are a little-endian machine it is sufficient to change the type
        // of the pointer to byte.
        let txdr8 = txdr as *mut u8;

        // Safety: we are dereferencing a pointer given to us by VolatileCell
        // (and thus UnsafeCell) using the same volatile access it would use.
        unsafe {
            txdr8.write_volatile(byte);
        }
    }

    pub fn recv32(&self) -> u32 {
        self.reg.rxdr.read().rxdr().bits()
    }

    /// Pulls one byte of data from the SPI RX FIFO.
    ///
    /// Preconditions:
    ///
    /// - There must be at least one byte of data in the FIFO (check using
    ///   `has_rx_byte` or call this in response to an RXP interrupt).
    ///
    /// - Frame size must be set to 8 bits or smaller. (Behavior if you write a
    ///   partial frame to the FIFO is not immediately clear from the
    ///   datasheet.)
    pub fn recv8(&self) -> u8 {
        // The RXDR register can be accessed as a byte, halfword, or word. This
        // determines how many bytes are pushed in. stm32h7/svd2rust don't
        // understand this, and so we have to get a pointer to the byte portion
        // of the register manually and dereference it.

        // See send8 for further rationale / ranting.

        // Safety: "Downcast" rxdr to a pointer to its sole member, whose type
        // we know because of our unholy source-code-reading powers.
        let rxdr: &vcell::VolatileCell<u32> =
            unsafe { core::mem::transmute(&self.reg.rxdr) };
        // vcell is more pleasant and will happily give us the pointer we want.
        let rxdr: *mut u32 = rxdr.as_ptr();
        // As we are a little-endian machine it is sufficient to change the type
        // of the pointer to byte.
        let rxdr8 = rxdr as *mut u8;

        // Safety: we are dereferencing a pointer given to us by VolatileCell
        // (and thus UnsafeCell) using the same volatile access it would use.
        unsafe { rxdr8.read_volatile() }
    }

    pub fn end(&self) {
        // Clear flags that tend to get set during transactions.
        self.reg.ifcr.write(|w| w.txtfc().set_bit());
        // Disable the transfer state machine.
        self.reg.cr1.modify(|_, w| w.spe().clear_bit());
        // Turn off interrupt enables.
        self.reg.ier.reset();

        // This is where we'd report errors (TODO). For now, just clear the
        // error flags, as they're sticky.
        self.reg.ifcr.write(|w| {
            w.ovrc()
                .set_bit()
                .udrc()
                .set_bit()
                .modfc()
                .set_bit()
                .tifrec()
                .set_bit()
        });
    }

    pub fn enable_transfer_interrupts(&self) {
        self.reg
            .ier
            .write(|w| w.txpie().set_bit().rxpie().set_bit().eotie().set_bit());
    }

    pub fn disable_can_tx_interrupt(&self) {
        self.reg.ier.modify(|_, w| w.txpie().clear_bit());
    }

    pub fn enable_can_tx_interrupt(&self) {
        self.reg.ier.modify(|_, w| w.txpie().set_bit());
    }

    pub fn check_eot(&self) -> bool {
        self.reg.sr.read().eot().is_completed()
    }

    pub fn clear_eot(&self) {
        self.reg.ifcr.write(|w| w.eotc().set_bit());
    }

    pub fn read_status(&self) -> u32 {
        self.reg.sr.read().bits()
    }

    pub fn check_overrun(&self) -> bool {
        self.reg.sr.read().ovr().is_overrun()
    }
}
