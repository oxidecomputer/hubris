// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32H7 QSPI low-level driver crate.

#![no_std]

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_qspi_api::Command;
use userlib::{sys_irq_control, sys_recv_notification};
use zerocopy::IntoBytes;

const FIFO_SIZE: usize = 32;
const FIFO_THRESH: usize = 16;

// In a perfect world we would use quad read everywhere because it is fast.
// We've seen some inconsistency with the quad read command on some targets
// so limit its use to targets where we've confirmed.
pub enum ReadSetting {
    Single,
    Quad,
}

/// Wrapper for a reference to the register block.
pub struct Qspi {
    reg: &'static device::quadspi::RegisterBlock,
    interrupt: u32,
    read_command: Command,
}

pub enum QspiError {
    Timeout,
    TransferError,
}

impl Qspi {
    /// Creates a new wrapper for `reg`.
    pub fn new(
        reg: &'static device::quadspi::RegisterBlock,
        interrupt: u32,
        read: ReadSetting,
    ) -> Self {
        Self {
            reg,
            interrupt,
            read_command: match read {
                ReadSetting::Single => Command::Read,
                ReadSetting::Quad => Command::QuadRead,
            },
        }
    }

    /// Sets up the QSPI controller with some canned settings.
    ///
    /// The controller must have clock enabled and be out of reset before
    /// calling this.
    ///
    /// You must call this before any other function on this `Qspi`.
    pub fn configure(&self, divider: u8, l2size: u8) {
        assert!(divider > 0);
        assert!(l2size > 0 && l2size < 64);

        #[rustfmt::skip]
        self.reg.cr.write(|w| unsafe {
            w
                // Divide kernel clock by the divider, which means setting
                // prescaler to one less.
                .prescaler().bits(divider - 1)
                // In both read and write modes we try to get 16 bytes into the
                // FIFO before bothering to wake up.
                .fthres().bits(FIFO_THRESH as u8 - 1)
                // On.
                .en().set_bit()
        });
        #[rustfmt::skip]
        self.reg.dcr.write(|w| unsafe {
            w
                // Flash size is recorded as log2 minus 1.
                .fsize().bits(l2size - 1)
                // CS high time: 1 cycle between (arbitrary)
                .csht().bits(1)
                // Clock mode 0.
                .ckmode().clear_bit()
        });
    }

    /// Reads the Device ID Data, a 20-byte sequence describing the Flash chip.
    /// This can be used to get basic details of the chip, and also to detect
    /// whether a chip is attached at all.
    pub fn read_id(&self, buf: &mut [u8; 20]) -> Result<(), QspiError> {
        self.read_impl(Command::ReadId, None, buf)
    }

    /// Reads the Device unique ID buffer
    ///
    /// This is implemented in terms of the Winbond part; other chips may
    /// require different commands.
    pub fn read_unique_id(&self, buf: &mut [u8; 12]) -> Result<(), QspiError> {
        self.read_impl(Command::ReadUniqueId, None, buf)
    }

    /// Reads the Status register.
    pub fn read_status(&self) -> Result<u8, QspiError> {
        let mut status = 0u8;
        self.read_impl(Command::ReadStatusReg, None, status.as_mut_bytes())?;
        Ok(status)
    }

    /// Reads from flash storage starting at `address` and continuing for
    /// `data.len()` bytes, depositing the bytes into `data`.
    pub fn read_memory(
        &self,
        address: u32,
        data: &mut [u8],
    ) -> Result<(), QspiError> {
        self.read_impl(self.read_command, Some(address), data)
    }

    /// Sets the Write Enable Latch on the flash chip, allowing a write/erase
    /// command sent immediately after to succeed.
    pub fn write_enable(&self) -> Result<(), QspiError> {
        self.write_impl(Command::WriteEnable, None, &[])
    }

    /// Performs a bulk erase of the chip. Note that this may take a rather long
    /// time -- about 8 minutes for a freshly purchased 32MiB Micron part -- and
    /// is very unpredictable, since it depends on how much has been written
    /// since last erase.
    ///
    /// Erasing a NAND flash chip resets all bits to 1.
    pub fn bulk_erase(&self) -> Result<(), QspiError> {
        self.write_impl(Command::BulkErase, None, &[])
    }

    /// Erases the 64kiB sector containing `addr`.
    ///
    /// This may take a bit of time, on the order of milliseconds. If you are
    /// erasing the entire chip, you may want `bulk_erase`.
    ///
    /// Erasing a sector of a NAND flash chip resets all bits to 1.
    pub fn sector_erase(&self, addr: u32) -> Result<(), QspiError> {
        self.write_impl(Command::SectorErase, Some(addr), &[])
    }

    /// Writes `data` into flash memory beginning at `addr`.
    ///
    /// Any zero bits in `data` will clear the corresponding bits in flash; any
    /// 1 bits will leave the flash bit unchanged. This is inherent to how NAND
    /// flash works. If the `data.len()` bytes starting at `addr` have been
    /// erased, they will contain 1s, and this will deposit a copy of `data`.
    ///
    /// It is sometimes (rarely) useful to deliberately overwrite data using
    /// this routine, to update information without erasing -- but of course it
    /// can only clear bits.
    pub fn page_program(
        &self,
        addr: u32,
        data: &[u8],
    ) -> Result<(), QspiError> {
        self.write_impl(Command::PageProgram, Some(addr), data)
    }

    /// Helper for error paths.
    fn disable_all_interrupts(&self) {
        self.reg.cr.modify(|_, w| {
            w.ftie()
                .clear_bit()
                .tcie()
                .clear_bit()
                .teie()
                .clear_bit()
                .toie()
                .clear_bit()
        });
    }

    fn write_impl(
        &self,
        command: Command,
        addr: Option<u32>,
        data: &[u8],
    ) -> Result<(), QspiError> {
        let result = self.write_impl_inner(command, addr, data);
        if result.is_err() {
            self.disable_all_interrupts();
        }
        result
    }

    /// Internal implementation of writes.
    fn write_impl_inner(
        &self,
        command: Command,
        addr: Option<u32>,
        data: &[u8],
    ) -> Result<(), QspiError> {
        if !data.is_empty() {
            self.set_transfer_length(data.len());
        }

        // Clear flags we'll use later.
        self.reg.fcr.write(|w| w.ctcf().set_bit());

        // Note: if we aren't using an address, this write will kick things off.
        // Otherwise it's the AR write below.
        #[rustfmt::skip]
        #[allow(clippy::bool_to_int_with_if)]
        self.reg.ccr.write(|w| unsafe {
            w
                // Indirect write
                .fmode().bits(0b00)
                // Data on single line, or no data
                .dmode().bits(if data.is_empty() { 0b00 } else { 0b01 })
                // Dummy cycles = 0 for this
                .dcyc().bits(0)
                // No alternate bytes
                .abmode().bits(0)
                // 32-bit address, if present.
                .adsize().bits(if addr.is_some() { 0b11 } else { 0b00 })
                // ...on one line for now, if present.
                .admode().bits(if addr.is_some() { 0b01 } else { 0b00 })
                // Instruction on single line
                .imode().bits(0b01)
                // And, the op
                .instruction().bits(command as u8)
        });
        if let Some(addr) = addr {
            self.reg.ar.write(|w| unsafe { w.address().bits(addr) });
        }

        // We're going to update this slice in place as we send data by lopping
        // off the front.
        let mut data = data;
        while !data.is_empty() {
            let sr = self.reg.sr.read();

            if sr.tof().bit_is_set() {
                self.reg.fcr.modify(|_, w| w.ctof().set_bit());
                return Err(QspiError::Timeout);
            }
            if sr.tef().bit_is_set() {
                self.reg.fcr.modify(|_, w| w.ctef().set_bit());
                return Err(QspiError::TransferError);
            }

            // Make sure our errors are enabled
            self.reg
                .cr
                .modify(|_, w| w.teie().set_bit().toie().set_bit());

            // How much space is in the FIFO?
            let fl = usize::from(sr.flevel().bits());
            let ffree = FIFO_SIZE - fl;
            if ffree >= FIFO_THRESH.min(data.len()) {
                // Calculate the write size. Note that this may be bigger than
                // the threshold used above above. We'll opportunistically
                // insert as much as we can.
                let immediate_write = ffree.min(data.len());
                let (chunk, new_data) = data.split_at(immediate_write);
                for &byte in chunk {
                    self.send8(byte);
                }
                data = new_data;
                continue;
            }

            // FIFO was observed to be too full to worry about. Time to wait.

            // During a write, we're only ever waiting for the FIFO to become
            // empty enough for data. The final, possibly smaller, chunk does
            // not require special handling.
            self.reg.cr.modify(|_, w| w.ftie().set_bit());

            // Interrupt-mediated poll of the status register.
            loop {
                // Unmask our interrupt.
                sys_irq_control(self.interrupt, true);
                // And wait for it to arrive.
                sys_recv_notification(self.interrupt);
                if self.reg.sr.read().ftf().bit() {
                    break;
                }
            }

            // Just loop back around to the fast path to avoid duplicating code
            // here.
        }

        // We're now interested in transfer complete, not FIFO ready.
        self.reg
            .cr
            .modify(|_, w| w.ftie().clear_bit().tcie().set_bit());
        while self.transfer_not_complete() {
            // Unmask our interrupt.
            sys_irq_control(self.interrupt, true);
            // And wait for it to arrive.
            sys_recv_notification(self.interrupt);
        }
        self.reg.cr.modify(|_, w| {
            w.tcie().clear_bit().teie().clear_bit().toie().clear_bit()
        });
        Ok(())
    }

    fn read_impl(
        &self,
        command: Command,
        addr: Option<u32>,
        out: &mut [u8],
    ) -> Result<(), QspiError> {
        let result = self.read_impl_inner(command, addr, out);
        if result.is_err() {
            self.disable_all_interrupts();
        }
        result
    }

    /// Internal implementation of reads.
    fn read_impl_inner(
        &self,
        command: Command,
        addr: Option<u32>,
        out: &mut [u8],
    ) -> Result<(), QspiError> {
        assert!(!out.is_empty());

        self.set_transfer_length(out.len());

        // Routine below expects that we don't have a transfer-complete flag
        // hanging around from some previous transfer -- ensure this:
        self.reg.fcr.write(|w| w.ctcf().set_bit());

        let (quad_setting, ddr_setting) = match command {
            Command::QuadRead => (true, false),
            Command::QuadDdrRead => (true, true),
            Command::DdrRead => (false, true),
            _ => (false, false),
        };

        #[rustfmt::skip]
        #[allow(clippy::bool_to_int_with_if)]
        self.reg.ccr.write(|w| unsafe {
            w
                // Set DDR mode if quad read
                .ddrm().bit(ddr_setting)
                .dhhc().bit(ddr_setting)
                // Indirect read
                .fmode().bits(0b01)
                // Data on single line, 4 lines if quad or no line
                .dmode().bits(if out.is_empty() { 0b00 } else if quad_setting { 0b11 } else { 0b01 } )
                // Dummy cycles = 0 for single read, 8 for quad
                .dcyc().bits(if quad_setting { 8 } else { 0 })
                // No alternate bytes
                .abmode().bits(0)
                // 32-bit address if present.
                .adsize().bits(if addr.is_some() { 0b11 } else { 0b00 })
                // ...on one line for now (or 4 for the DDR command), if present.
                .admode().bits(if addr.is_some() { if ddr_setting { 0b11} else { 0b01 } } else { 0b00 })
                // Instruction on single line
                .imode().bits(0b01)
                // And, the op
                .instruction().bits(command as u8)
        });
        if let Some(addr) = addr {
            self.reg.ar.write(|w| unsafe { w.address().bits(addr) });
        }

        // We're going to shorten this slice by lopping off the front as we
        // perform transfers.
        let mut out = out;
        while !out.is_empty() {
            let sr = self.reg.sr.read();

            if sr.tof().bit_is_set() {
                self.reg.fcr.modify(|_, w| w.ctof().set_bit());
                return Err(QspiError::Timeout);
            }
            if sr.tef().bit_is_set() {
                self.reg.fcr.modify(|_, w| w.ctef().set_bit());
                return Err(QspiError::TransferError);
            }
            // Make sure our errors are enabled
            self.reg
                .cr
                .modify(|_, w| w.teie().set_bit().toie().set_bit());
            // Is there enough to read that we want to bother with it?
            let fl = usize::from(sr.flevel().bits());
            if fl < FIFO_THRESH.min(out.len()) {
                // Nope! Let's wait for more bytes.

                // Figure out which event we're looking for and turn it on.
                if out.len() >= FIFO_THRESH {
                    // We want the FIFO fill event
                    self.reg.cr.modify(|_, w| w.ftie().set_bit());
                } else {
                    // We want the transfer-complete event
                    #[rustfmt::skip]
                    self.reg.cr.modify(|_, w|
                        w.ftie().clear_bit()
                        .tcie().set_bit()
                    );
                }

                // Unmask our interrupt.
                sys_irq_control(self.interrupt, true);
                // And wait for it to arrive.
                sys_recv_notification(self.interrupt);

                // Try the check again. We may retry the check on spurious
                // wakeups, but, spurious wakeups are expected to be pretty
                // unusual, and the check isn't that expensive -- so, why add
                // extra logic.
                continue;
            }

            // Okay! We have some data! Let's evacuate it.

            // Calculate the read size. Note that, if we have more bytes left to
            // read than FIFO_THRESH, this may be bigger than FIFO_THRESH. We'll
            // opportunistically take however much remains.
            let read_size = fl.min(out.len());
            let (dest, new_out) = out.split_at_mut(read_size);
            for byte in dest {
                *byte = self.recv8();
            }
            out = new_out;

            // next!
        }

        // Wait for the Transfer Complete flag to get set. This does not
        // necessarily imply the BUSY flag is clear, but since commands are
        // issued into a FIFO, we can issue the next command even while BUSY is
        // set, it appears.
        self.reg
            .cr
            .modify(|_, w| w.ftie().clear_bit().tcie().set_bit());
        while self.transfer_not_complete() {
            // Unmask our interrupt.
            sys_irq_control(self.interrupt, true);
            // And wait for it to arrive.
            sys_recv_notification(self.interrupt);
        }

        // Clean up by disabling our interrupt sources.
        self.reg.cr.modify(|_, w| {
            w.ftie()
                .clear_bit()
                .tcie()
                .clear_bit()
                .teie()
                .clear_bit()
                .toie()
                .clear_bit()
        });
        Ok(())
    }

    fn set_transfer_length(&self, len: usize) {
        assert!(len != 0);
        self.reg
            .dlr
            .write(|w| unsafe { w.dl().bits(len as u32 - 1) });
    }

    fn transfer_not_complete(&self) -> bool {
        !self.reg.sr.read().tcf().bit()
    }

    /// Performs an 8-bit load from the low byte of the Data Register.
    ///
    /// The DR is access-size-sensitive, so despite being 32 bits wide, if you
    /// want to remove only one byte from the FIFO, you need to use an 8-bit
    /// access.
    ///
    /// You _really_ want to make sure the FIFO has some contents before calling
    /// this. Otherwise you'll get garbage, not an error.
    fn recv8(&self) -> u8 {
        let dr: &vcell::VolatileCell<u32> =
            unsafe { core::mem::transmute(&self.reg.dr) };
        // vcell is more pleasant and will happily give us the pointer we want.
        let dr: *mut u32 = dr.as_ptr();
        // As we are a little-endian machine it is sufficient to change the type
        // of the pointer to byte.
        let dr8 = dr as *mut u8;

        // Safety: we are dereferencing a pointer given to us by VolatileCell
        // (and thus UnsafeCell) using the same volatile access it would use.
        unsafe { dr8.read_volatile() }
    }

    /// Performs an 8-bit store to the low byte of the Data Register.
    ///
    /// The DR is access-size-sensitive, so despite being 32 bits wide, if you
    /// want to put only one byte into the FIFO, you need to use an 8-bit
    /// access.
    ///
    /// You _really_ want to make sure the FIFO has spare space before calling
    /// this. It's not totally clear what will happen otherwise, but at least
    /// one byte will definitely get dropped.
    fn send8(&self, b: u8) {
        let dr: &vcell::VolatileCell<u32> =
            unsafe { core::mem::transmute(&self.reg.dr) };
        // vcell is more pleasant and will happily give us the pointer we want.
        let dr: *mut u32 = dr.as_ptr();
        // As we are a little-endian machine it is sufficient to change the type
        // of the pointer to byte.
        let dr8 = dr as *mut u8;

        // Safety: we are dereferencing a pointer given to us by VolatileCell
        // (and thus UnsafeCell) using the same volatile access it would use.
        unsafe {
            dr8.write_volatile(b);
        }
    }
}
