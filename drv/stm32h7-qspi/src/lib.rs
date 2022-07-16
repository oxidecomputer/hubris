// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32H7 QSPI low-level driver crate.

#![no_std]

// Note that stm32h7b3 has QUADSPI support also.

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use userlib::{sys_irq_control, sys_recv_closed, TaskId};
use zerocopy::AsBytes;

const FIFO_SIZE: usize = 32;
const FIFO_THRESH: usize = 16;

/// Wrapper for a reference to the register block.
pub struct Qspi {
    reg: &'static device::quadspi::RegisterBlock,
    interrupt: u32,
}

cfg_if::cfg_if! {
    if #[cfg(not(any(feature = "micron", feature = "winbond")))]  {
        compile_error!{"Must select a flash chip mfg [micron|winbond]"}
    }
}

enum Command {
    ReadStatusReg = 0x05,
    WriteEnable = 0x06,
    PageProgram = 0x12,
    Read = 0x13,

    // Note, There are multiple ReadId commands.
    // Gimlet and Gemini's flash parts both respond to 0x9F.
    // Gemini's does not respond to 0x9E (returns all zeros).
    // TODO: Proper flash chip quirk support.
    ReadId = 0x9F,

    BulkErase = 0xC7,
    SectorErase = 0xDC,

    // Micron and Winbond implement the same QuadOutputFast Read commands.
    _QuadOutputFastRead3B = 0x6B,
    QuadOutputFastRead4B = 0x6C,

    #[cfg(feature = "winbond")]
    _ReadStatusReg2 = 0x35,
    #[cfg(feature = "winbond")]
    _ReadStatusReg3 = 0x15,
    #[cfg(feature = "winbond")]
    _WriteStatusReg = 0x01,
    #[cfg(feature = "winbond")]
    WriteStatusReg2 = 0x31,
    #[cfg(feature = "winbond")]
    WriteEnableVolatileSR = 0x50,
    #[cfg(feature = "winbond")]
    _WriteStatusReg3 = 0x11,

    #[cfg(feature = "micron")]
    EnterQuadIOMode = 0x35,
    #[cfg(feature = "micron")]
    ResetQuadIOMode = 0xF5,

    }

cfg_if::cfg_if! {
    if #[cfg(feature = "winbond")] {
        const _WINBOND_SR2_SUS: u8 =  1 << (15 - 8);   // Suspend Status, S15 - base-bit
        const _WINBOND_SR2_CMP: u8 =  1 << (14 - 8);   // Complement Protect
        const _WINBOND_SR2_LB3: u8 =  1 << (13 - 8);   // Security Register Lock Bits
        const _WINBOND_SR2_LB2: u8 =  1 << (12 - 8);   //
        const _WINBOND_SR2_LB1: u8 =  1 << (11 - 8);   //
        const _WINBOND_SR2_RES: u8 =  1 << (10 - 8);   // Reserved
        const WINBOND_SR2_QE: u8 =   1 << (9 - 8);    // Quad Enable
        const _WINBOND_SR2_SRP1: u8 = 1 << (8 - 8);    // Status Register Protect 1
    }
}

impl From<Command> for u8 {
    fn from(c: Command) -> u8 {
        c as u8
    }
}

impl Qspi {
    /// Creates a new wrapper for `reg`.
    pub fn new(
        reg: &'static device::quadspi::RegisterBlock,
        interrupt: u32,
    ) -> Self {
        Self { reg, interrupt }
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
    pub fn read_id(&self, buf: &mut [u8; 20]) {
        self.read_impl(Command::ReadId, None, buf)
    }

    /// Reads the Status register.
    pub fn read_status(&self) -> u8 {
        let mut status = 0u8;
        self.read_impl(Command::ReadStatusReg, None, status.as_bytes_mut());
        status
    }

    /// Reads from flash storage starting at `address` and continuing for
    /// `data.len()` bytes, depositing the bytes into `data`.
    pub fn read_memory(&self, address: u32, data: &mut [u8]) {
        self.read_impl(Command::Read, Some(address), data);
    }

    /// Reads from flash storage starting at `address` and continuing for
    /// `data.len()` bytes, depositing the bytes into `data`.
    pub fn fast_read_memory(&self, address: u32, data: &mut [u8]) {
        self.quad_read_impl(Command::QuadOutputFastRead4B, Some(address), data);
    
    }

    /// Sets the Write Enable Latch on the flash chip, allowing a write/erase
    /// command sent immediately after to succeed.
    pub fn write_enable(&self) {
        self.write_impl(Command::WriteEnable, None, &[])
    }

    /// Enter Quad-mode read mode (I/O on all four lines)
    #[cfg(feature = "micron")]
    pub fn quad_read_enter(&self) {
        self.write_impl(Command::EnterQuadIOMode, None, &[])
    }

    /// Exit Quad-mode read mode (I/O on all four lines)
    #[cfg(feature = "micron")]
    pub fn quad_read_exit(&self) {
        self.write_impl(Command::ResetQuadIOMode, None, &[])
    }

    /// Enter Quad-mode read mode (I/O on all four lines)
    #[cfg(feature = "winbond")]
    pub fn quad_read_enter(&self) {
        self.write_impl(Command::WriteEnableVolatileSR, None, &[]);
        // XXX This write should be a read/modify/write, but we aren't using
        // any other bits in SR1.
        self.write_impl(Command::WriteStatusReg2, None, &[WINBOND_SR2_QE]);
        // XXX What if quad_read_exit does not get called due to task restart or
        // some unforseen loss of sync with the flash chip?
    }

    /// Exit Quad-mode read mode (I/O on all four lines)
    #[cfg(feature = "winbond")]
    pub fn quad_read_exit(&self) {
        self.write_impl(Command::WriteEnableVolatileSR, None, &[]);
        // XXX This write should be a read/modify/write, but we aren't using
        // any other bits in SR1.
        self.write_impl(Command::WriteStatusReg2, None, &[0x00]);
    }

    /// Performs a bulk erase of the chip. Note that this may take a rather long
    /// time -- about 8 minutes for a freshly purchased 32MiB Micron part -- and
    /// is very unpredictable, since it depends on how much has been written
    /// since last erase.
    ///
    /// Erasing a NAND flash chip resets all bits to 1.
    pub fn bulk_erase(&self) {
        self.write_impl(Command::BulkErase, None, &[])
    }

    /// Erases the 64kiB sector containing `addr`.
    ///
    /// This may take a bit of time, on the order of milliseconds. If you are
    /// erasing the entire chip, you may want `bulk_erase`.
    ///
    /// Erasing a sector of a NAND flash chip resets all bits to 1.
    pub fn sector_erase(&self, addr: u32) {
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
    pub fn page_program(&self, addr: u32, data: &[u8]) {
        self.write_impl(Command::PageProgram, Some(addr), data)
    }

    /// Internal implementation of writes.
    fn write_impl(&self, command: Command, addr: Option<u32>, data: &[u8]) {
        if !data.is_empty() {
            self.set_transfer_length(data.len());
        }

        // Clear flags we'll use later.
        self.reg.fcr.write(|w| w.ctcf().set_bit());

        // Note: if we aren't using an address, this write will kick things off.
        // Otherwise it's the AR write below.
        #[rustfmt::skip]
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
            // How much space is in the FIFO?
            let fl = usize::from(self.reg.sr.read().flevel().bits());
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
                let _rm =
                    sys_recv_closed(&mut [], self.interrupt, TaskId::KERNEL)
                        .unwrap();
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
        while self.is_busy() {
            // Unmask our interrupt.
            sys_irq_control(self.interrupt, true);
            // And wait for it to arrive.
            let _rm = sys_recv_closed(&mut [], self.interrupt, TaskId::KERNEL)
                .unwrap();
        }
        self.reg.cr.modify(|_, w| w.tcie().clear_bit());
    }

    /// Internal implementation of reads.
    fn read_impl(&self, command: Command, addr: Option<u32>, out: &mut [u8]) {
        assert!(!out.is_empty());

        self.set_transfer_length(out.len());

        // Routine below expects that we don't have a transfer-complete flag
        // hanging around from some previous transfer -- ensure this:
        self.reg.fcr.write(|w| w.ctcf().set_bit());

        #[rustfmt::skip]
        self.reg.ccr.write(|w| unsafe {
            w
                // Indirect read
                .fmode().bits(0b01)
                // Data on single line, or no data
                .dmode().bits(if out.is_empty() { 0b00 } else { 0b01 })
                // Dummy cycles = 0 for this
                .dcyc().bits(0)
                // No alternate bytes
                .abmode().bits(0)
                // 32-bit address if present.
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

        // We're going to shorten this slice by lopping off the front as we
        // perform transfers.
        let mut out = out;
        while !out.is_empty() {
            // Is there enough to read that we want to bother with it?
            let fl = usize::from(self.reg.sr.read().flevel().bits());
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
                let _rm =
                    sys_recv_closed(&mut [], self.interrupt, TaskId::KERNEL)
                        .unwrap();

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

        // There's a chance we race BUSY clearing here, because we've seen it
        // happen in the wild, no matter what the reference manual might
        // suggest. Waiting for transfer complete seems to be good enough,
        // though the relationship between BUSY and TC is not documented.
        self.reg
            .cr
            .modify(|_, w| w.ftie().clear_bit().tcie().set_bit());
        while self.is_busy() {
            // Unmask our interrupt.
            sys_irq_control(self.interrupt, true);
            // And wait for it to arrive.
            let _rm = sys_recv_closed(&mut [], self.interrupt, TaskId::KERNEL)
                .unwrap();
        }

        // Clean up by disabling our interrupt sources.
        self.reg
            .cr
            .modify(|_, w| w.ftie().clear_bit().tcie().clear_bit());
    }

    /// Internal implementation of fast reads.
    // XXX Copy/Paste/edit from read_impl()
    // XXX This is only tested with the QuadOutputFastRead4B command on Winbond
    // XXX For winbond: W25Q256FV 
    // XXX Q: Do I need to use the Enter QPI command (38h)? Winbond 6.1.4
    // XXX A: No. QPI uses 4-wires for the instruction (ExitQPI=0xff).
    //
    //   "The Quad Enable (QE) bit in Status Register-2 must be set to 1 before the
    //    device will accept the Fast Read Quad Output Instruction."
    //
    //    The Fast Read Quad Output instruction can operate at the highest
    //    possible frequency of FR (see AC Electrical Characteristics).
    //    This is accomplished by adding eight “dummy” clocks after the
    //    24/32-bit address as shown in Figure 20.
    //    The dummy clocks allow the device's internal circuits additional
    //    time for setting up the initial address.
    //    The input data during the dummy clocks is “don’t care”.
    //    However, the IO pins should be high-impedance prior to the falling
    //    edge of the first data out clock. 
    //
    //    - Instruction in single wire MOSI mode.
    //    - 32-bit address in single wire MOSI mode.
    //    - 8 dummy clocks.
    //    - Data in four-wire MISO mode.
    //
    //    Winbond Status Register 1:
    //      - S7, SRP0, Status Register Protect 0, Volatile/Non-Volatile Writable
    //      - S6, TB, Tom/Bottom Protect Bit, Volatile/Non-Volatile Writable
    //      - S5, BP3, Block Protect Bits, Volatile/Non-Volatile Writable
    //      - S4, BP2, Block Protect Bits, Volatile/Non-Volatile Writable
    //      - S3, BP1, Block Protect Bits, Volatile/Non-Volatile Writable
    //      - S2, BP0, Block Protect Bits, Volatile/Non-Volatile Writable
    //      - S1, WEL, Write Enable Latch, Status-Only
    //      - S0, BUSY, Erase/Write In Progress, Status-Only
    //    Winbond Status Register 2:
    //      - S15, SUS, Suspend Status, RO
    //      - S14, CMP, Complement Protect, Volatile/Non-Volatile Writable
    //      - S13, LB3, Security Register Lock Bits, Volatile/Non-Volatile OTP Writable
    //      - S12, LB2, Security Register Lock Bits, Volatile/Non-Volatile OTP Writable
    //      - S11, LB1, Security Register Lock Bits, Volatile/Non-Volatile OTP Writable
    //      - S10, RES, Reserved
    //      - S9, QE, Quad Enable, Volatile/Non-Volatile OTP Writable
    //      - S8, SRP1, Status Register Protect 1, Volatile/Non-Volatile Writable
    //
    // Micron
    //   - Enter Quad Input/Output Mode 35h
    //   - Reset Quad Input/Output Mode F5h
    //   - CRC Command Sequence on Entire Device (Bit 4 of STATUS register indicates pass/fail)
    //       - 1: 9Bh Command code for interface activation
    //       - 2: 27h Sub-command code for CRC operation
    //       - 3: FFh CRC operation option selection (CRC operation on entire device)
    //       - 4: CRC[7:0] 1st byte of expected CRC value
    //       - 5-10: CRC[55:8] 2nd to 7th bytes of expected CRC value
    //       - 11: CRC[63:56] 8th byte of expected CRC value
    //       - Drive S# High, operation sequence confirmed; CRC operation starts
    //
    //   - CRC Command Sequence on an address range (Bit 4 of STATUS register indicates pass/fail)
    //       - 1: 9Bh Command code for interface activation
    //       - 2: 27h Sub-command code for CRC operation
    //       - 3: FEh CRC operation option selection (CRC operation on entire device)
    //       - 4: CRC[7:0] 1st byte of expected CRC value
    //       - 5-10: CRC[55:8] 2nd to 7th bytes of expected CRC value
    //       - 11: CRC[63:56] 8th byte of expected CRC value
    //       - 12: Start Address [7:0] Specifies the starting byte address for CRC operation
    //       - 13-14: Start Address [23:8]
    //       - 15: Start Address [31:24]
    //       - 16: End Address [7:0] Specifies the ending byte address for CRC operation
    //       - 17-17: End Address [23:8]
    //       - 19: End Address [31:24]
    //       - Drive S# High, operation sequence confirmed; CRC operation starts
    //

    fn quad_read_impl(&self, command: Command, addr: Option<u32>, out: &mut [u8]) {
        assert!(!out.is_empty());

        self.quad_read_enter();
        self.set_transfer_length(out.len());

        // Routine below expects that we don't have a transfer-complete flag
        // hanging around from some previous transfer -- ensure this:
        self.reg.fcr.write(|w| w.ctcf().set_bit());

        #[rustfmt::skip]
        self.reg.ccr.write(|w| unsafe {
            w
                // Indirect read
                .fmode().bits(0b01)
                // Instruction on single line
                .imode().bits(0b01)
                // 32-bit address if present.
                .adsize().bits(if addr.is_some() { 0b11 } else { 0b00 })
                // ...on one line for now, if present.
                .admode().bits(if addr.is_some() { 0b01 } else { 0b00 })
                // No alternate bytes
                .abmode().bits(0)
                // Dummy cycles = 8 for this
                .dcyc().bits(8)
                // Data on four lines, or no data
                .dmode().bits(if out.is_empty() { 0b00 } else { 0b11 })
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
            // Is there enough to read that we want to bother with it?
            let fl = usize::from(self.reg.sr.read().flevel().bits());
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
                let _rm =
                    sys_recv_closed(&mut [], self.interrupt, TaskId::KERNEL)
                        .unwrap();

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

        // There's a chance we race BUSY clearing here, because we've seen it
        // happen in the wild, no matter what the reference manual might
        // suggest. Waiting for transfer complete seems to be good enough,
        // though the relationship between BUSY and TC is not documented.
        self.reg
            .cr
            .modify(|_, w| w.ftie().clear_bit().tcie().set_bit());
        while self.is_busy() {
            // Unmask our interrupt.
            sys_irq_control(self.interrupt, true);
            // And wait for it to arrive.
            let _rm = sys_recv_closed(&mut [], self.interrupt, TaskId::KERNEL)
                .unwrap();
        }

        // Clean up by disabling our interrupt sources.
        self.reg
            .cr
            .modify(|_, w| w.ftie().clear_bit().tcie().clear_bit());
        self.quad_read_exit();
    }

    fn set_transfer_length(&self, len: usize) {
        assert!(len != 0);
        self.reg
            .dlr
            .write(|w| unsafe { w.dl().bits(len as u32 - 1) });
    }

    fn is_busy(&self) -> bool {
        self.reg.sr.read().busy().bit()
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
