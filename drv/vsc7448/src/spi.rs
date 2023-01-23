// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Vsc7448Rw, VscError};
use drv_spi_api::{SpiDevice, SpiServer};
use ringbuf::*;
use userlib::UnwrapLite;
use vsc7448_pac::{types::RegisterAddress, *};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Read { addr: u32, value: u32 },
    Write { addr: u32, value: u32 },
}

/// This indicates how many bytes we pad between (writing) the address bytes
/// and (reading) data back, during SPI transactions to the VSC7448.  See
/// section 5.5.2 for details.  1 padding byte should be good up to 6.5 MHz
/// SPI clock.
pub const SPI_NUM_PAD_BYTES: usize = 1;

ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

/// Helper struct to read and write from the VSC7448 over SPI
pub struct Vsc7448Spi<S: SpiServer>(SpiDevice<S>);
impl<S: SpiServer> Vsc7448Spi<S> {
    pub fn new(spi: SpiDevice<S>) -> Self {
        Self(spi)
    }

    #[inline(never)]
    fn read_core(&self, orig_addr: u32) -> Result<u32, VscError> {
        if !(0x71000000..0x72000000).contains(&orig_addr) {
            return Err(VscError::BadRegAddr(orig_addr));
        }

        // Section 5.5.2 of the VSC7448 datasheet specifies how to convert
        // a register address to a request over SPI.
        let addr = (orig_addr & 0x00FFFFFF) >> 2;
        let data: &[u8] = &addr.to_be_bytes()[1..];

        // We read back 7 + padding bytes in total:
        // - 3 bytes of address
        // - Some number of padding bytes
        // - 4 bytes of data
        const SIZE: usize = 7 + SPI_NUM_PAD_BYTES;
        let mut out = [0; SIZE];
        self.0.exchange(data, &mut out[..])?;
        let value =
            u32::from_be_bytes(out[SIZE - 4..].try_into().unwrap_lite());

        ringbuf_entry!(Trace::Read {
            addr: orig_addr,
            value
        });
        // The VSC7448 is configured to return 0x88888888 if a register is
        // read too fast.  Reading takes place over SPI: we write a 3-byte
        // address, then read 4 bytes of data; based on SPI speed, we may
        // need to configure padding bytes in between the address and
        // returning data.
        //
        // This is controlled by setting DEVCPU_ORG:IF_CFGSTAT.IF_CFG in
        // init(), then by padding bytes in the `out` arrays in
        // [read] and [write].
        //
        // Therefore, we should only read "too fast" if someone has modified
        // the SPI speed without updating the padding byte, which should
        // never happen in well-behaved code.
        //
        // If we see this sentinel value, then we check
        // DEVCPU_ORG:IF_CFGSTAT.IF_STAT.  If that bit is set, then the sentinel
        // value _actually_ indicates an error (and not just an unfortunate
        // coincidence).
        if value == 0x88888888 {
            // Panic immediately if we got an invalid read sentinel while
            // reading IF_CFGSTAT itself, because that means something has
            // gone very deeply wrong.  This check also protects us from a
            // stack overflow.
            let if_cfgstat = DEVCPU_ORG().DEVCPU_ORG().IF_CFGSTAT();
            if orig_addr == if_cfgstat.addr {
                panic!("Invalid nested read sentinel");
            }
            // This read should _never_ fail for timing reasons because the
            // DEVCPU_ORG register block can be accessed faster than all other
            // registers (section 5.3.2 of the datasheet).
            let v = self.read(if_cfgstat)?;
            if v.if_stat() == 1 {
                return Err(VscError::InvalidRegisterRead(orig_addr));
            }
        }
        Ok(value)
    }

    #[inline(never)]
    fn write_core(&self, reg_addr: u32, value: u32) -> Result<(), VscError> {
        if !(0x71000000..0x72000000).contains(&reg_addr) {
            return Err(VscError::BadRegAddr(reg_addr));
        }

        let addr = (reg_addr & 0x00FFFFFF) >> 2;
        let mut data: [u8; 7] = [0; 7];
        data[..3].copy_from_slice(&addr.to_be_bytes()[1..]);
        data[3..].copy_from_slice(&value.to_be_bytes());
        data[0] |= 0x80; // Indicates that this is a write

        ringbuf_entry!(Trace::Write {
            addr: reg_addr,
            value,
        });
        self.0.write(&data[..])?;
        Ok(())
    }
}

impl<S: SpiServer> Vsc7448Rw for Vsc7448Spi<S> {
    /// Reads from a VSC7448 register.  The register must be in the switch
    /// core register block, i.e. having an address in the range
    /// 0x71000000-0x72000000; otherwise, this return an error.
    #[inline(always)]
    fn read<T>(&self, reg: RegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u32>,
    {
        Ok(self.read_core(reg.addr)?.into())
    }

    /// Writes to a VSC7448 register.  This will overwrite the entire register;
    /// if you want to modify it, then use [Self::modify] instead.
    ///
    /// The register must be in the switch core register block, i.e. having an
    /// address in the range 0x71000000-0x72000000; otherwise, this will
    /// return an error.
    #[inline(always)]
    fn write<T>(
        &self,
        reg: RegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u32: From<T>,
    {
        self.write_core(reg.addr, u32::from(value))
    }
}
