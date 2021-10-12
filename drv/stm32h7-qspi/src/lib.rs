//! STM32H7 QSPI low-level driver crate.

#![no_std]

use stm32h7::stm32h743 as device;
use zerocopy::AsBytes;

/// Wrapper for a reference to the register block.
pub struct Qspi {
    reg: &'static device::quadspi::RegisterBlock,
}

enum Command {
    ReadStatusReg = 0x05,
    WriteEnable = 0x06,
    PageProgram = 0x12,
    Read = 0x13,

    ReadId = 0x9E,

    BulkErase = 0xC7,
    SectorErase = 0xDC,
}

impl From<Command> for u8 {
    fn from(c: Command) -> u8 {
        c as u8
    }
}

impl Qspi {
    /// Creates a new wrapper for `reg`.
    pub fn new(reg: &'static device::quadspi::RegisterBlock) -> Self {
        Self { reg }
    }

    /// Sets up the QSPI controller with some canned settings.
    ///
    /// The controller must have clock enabled and be out of reset before
    /// calling this.
    ///
    /// You must call this before any other function on this `Qspi`.
    pub fn configure(&self) {
        #[rustfmt::skip]
        self.reg.cr.write(|w| unsafe {
            w
                // Divide kernel clock by 128.
                .prescaler().bits(127)
                // On.
                .en().set_bit()
        });
        #[rustfmt::skip]
        self.reg.dcr.write(|w| unsafe {
            w
                // 2^25 = 32MiB = 256Mib
                .fsize().bits(24)
                // CS high time: 8 cycles between (arbitrary)
                .csht().bits(7)
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

    /// Sets the Write Enable Latch on the flash chip, allowing a write/erase
    /// command sent immediately after to succeed.
    pub fn write_enable(&self) {
        self.write_impl(Command::WriteEnable, None, &[])
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
        for &byte in data {
            self.send8(byte);
        }

        // TODO polling interface
        while self.is_busy() {
            // spin
        }
    }

    /// Internal implementation of reads.
    fn read_impl(&self, command: Command, addr: Option<u32>, out: &mut [u8]) {
        assert!(!out.is_empty());

        self.set_transfer_length(out.len());
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
        for byte in out.iter_mut() {
            loop {
                // TODO this will get replaced by a wait for a FIFO threshold
                // interrupt.
                if let Some(b) = self.try_recv() {
                    *byte = b;
                    break;
                }
            }
        }

        // TODO polling interface

        while self.is_busy() {
            // spin
        }
    }

    fn set_transfer_length(&self, len: usize) {
        assert!(len != 0);
        self.reg
            .dlr
            .write(|w| unsafe { w.dl().bits(len as u32 - 1) });
    }

    fn try_recv(&self) -> Option<u8> {
        if self.reg.sr.read().flevel() == 0 {
            None
        } else {
            Some(self.recv8())
        }
    }

    fn is_busy(&self) -> bool {
        self.reg.sr.read().busy().bit()
    }

    /// Performs an 8-bit load from the low byte of the Data Register.
    ///
    /// The DR is access-size-sensitive, so despite being 32 bits wide, if you
    /// want to remove only one byte from the FIFO, you need to use an 8-bit
    /// access.
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
