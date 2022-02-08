// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
#![no_std]

use drv_spi_api::{SpiDevice, SpiError};
use drv_stm32xx_sys_api::{self as sys_api, PinSet, Sys};
use ringbuf::*;
use userlib::hl::sleep_for;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Read(Register, u16),
    Write(Register, u16),
    Id(u16),
}
ringbuf!(Trace, 16, Trace::None);

/// Data from a management information base (MIB) counter on the chip,
/// used to monitor port activity for network management.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MIBCounter {
    Invalid,
    Count(u32),
    CountOverflow(u32),
}

/// Offsets used to access MIB counters
/// (see Table 4-200 in the datasheet for details)
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MIBOffset {
    /// Rx lo-priority (default) octet count, including bad packets.
    RxLoPriorityByte = 0x0,

    /// Rx hi-priority octet count, including bad packets.
    RxHiPriorityByte = 0x1,

    /// Rx undersize packets with good CRC.
    RxUndersizePkt = 0x2,

    /// Rx fragment packets with bad CRC, symbol errors or alignment errors.
    RxFragments = 0x3,

    /// Rx oversize packets with good CRC (maximum: 2000 bytes).
    RxOversize = 0x4,

    /// Rx packets longer than 1522 bytes with either CRC errors, alignment errors, or symbol errors (depends on max packet size setting).
    RxJabbers = 0x5,

    /// Rx packets w/ invalid data symbol and legal packet size.
    RxSymbolError = 0x6,

    /// Rx packets within (64,1522) bytes w/ an integral number of bytes and a bad CRC (upper limit depends on maximum packet size setting).
    RxCRCError = 0x7,

    /// Rx packets within (64,1522) bytes w/ a non-integral number of bytes and a bad CRC (upper limit depends on maximum packet size setting).
    RxAlignmentError = 0x8,

    /// Number of MAC control frames received by a port with 88-08h in Ether- Type field.
    RxControl8808Pkts = 0x9,

    /// Number of PAUSE frames received by a port. PAUSE frame is qualified with EtherType (88-08h), DA, control opcode (00-01), data length (64B minimum), and a valid CRC.
    RxPausePkts = 0xA,

    /// Rx good broadcast packets (not including error broadcast packets or valid multicast packets).
    RxBroadcast = 0xB,

    /// Rx good multicast packets (not including MAC control frames, error multicast packets or valid broadcast packets).
    RxMulticast = 0xC,

    /// Rx good unicast packets.
    RxUnicast = 0xD,

    /// Total Rx packets (bad packets included) that were 64 octets in length.
    Rx64Octets = 0xE,

    /// Total Rx packets (bad packets included) that are between 65 and 127 octets in length.
    Rx65to127Octets = 0xF,

    /// Total Rx packets (bad packets included) that are between 128 and 255 octets in length.
    Rx128to255Octets = 0x10,

    /// Total Rx packets (bad packets included) that are between 256 and 511 octets in length.
    Rx256to511Octets = 0x11,

    /// Total Rx packets (bad packets included) that are between 512 and 1023 octets in length.
    Rx512to1023Octets = 0x12,

    /// Total Rx packets (bad packets included) that are between 1024 and 2000 octets in length (upper limit depends on max packet size setting).
    Rx1024to2000Octets = 0x13,

    /// Tx lo-priority good octet count, including PAUSE packets.
    TxLoPriorityByte = 0x14,

    /// Tx hi-priority good octet count, including PAUSE packets.
    TxHiPriorityByte = 0x15,

    /// The number of times a collision is detected later than 512 bit-times into the Tx of a packet.
    TxLateCollision = 0x16,

    /// Number of PAUSE frames transmitted by a port.
    TxPausePkts = 0x17,

    /// Tx good broadcast packets (not including error broadcast or valid multi- cast packets).
    TxBroadcastPkts = 0x18,

    /// Tx good multicast packets (not including error multicast packets or valid broadcast packets).
    TxMulticastPkts = 0x19,

    /// Tx good unicast packets.
    TxUnicastPkts = 0x1A,

    /// Tx packets by a port for which the 1st Tx attempt is delayed due to the busy medium.
    TxDeferred = 0x1B,

    /// Tx total collision, half duplex only.
    TxTotalCollision = 0x1C,

    /// A count of frames for which Tx fails due to excessive collisions.
    TxExcessiveCollision = 0x1D,

    /// Successfully Tx frames on a port for which Tx is inhibited by exactly one collision.
    TxSingleCollision = 0x1E,

    /// Successfully Tx frames on a port for which Tx is inhibited by more than one collision.
    TxMultipleCollision = 0x1F,
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(non_camel_case_types)]
pub enum Register {
    /// Chip ID and enable register
    CIDER = 0x0,
    /// Switch global control register 1
    SGCR1 = 0x2,
    /// Switch global control register 2
    SGCR2 = 0x4,
    /// Switch global control register 3
    SGCR3 = 0x6,
    /// Switch global control register 6
    SGCR6 = 0xc,
    /// Switch global control register 7
    SGCR7 = 0xe,
    /// MAC address register 1
    MACAR1 = 0x10,
    /// MAC address register 2
    MACAR2 = 0x12,
    /// MAC address register 3
    MACAR3 = 0x14,

    /// Indirect access data register 4
    IADR4 = 0x02c,
    /// Indirect access data register 5
    IADR5 = 0x02e,
    /// Indirect access control register
    IACR = 0x030,

    /// PHY 1 and MII basic control register
    P1MBCR = 0x4c,
    /// PHY 1 and MII basic status register
    P1MBSR = 0x4e,

    /// PHY 2 and MII basic control register
    P2MBCR = 0x58,
    /// PHY 2 and MII basic status register
    P2MBSR = 0x5a,

    /// PHY 1 special control and status register
    P1PHYCTRL = 0x066,
    /// PHY 2 special control and status register
    P2PHYCTRL = 0x06a,

    /// Configuration status and serial bus mode register
    CFGR = 0xd8,

    /// DSP control 1 register
    DSP_CNTRL_6 = 0x734,
}

pub enum ResetSpeed {
    Slow,
    Normal,
}
pub struct Ksz8463 {
    spi: SpiDevice,
    nrst: PinSet,
    reset_speed: ResetSpeed,
}

impl Ksz8463 {
    pub fn new(spi: SpiDevice, nrst: PinSet, reset_speed: ResetSpeed) -> Self {
        Self {
            spi,
            nrst,
            reset_speed,
        }
    }

    fn pack_addr(address: u16) -> u16 {
        // This chip has a bizarre addressing scheme where you specify the
        // address with 4-byte resolution (i.e. masking off the lower two bits
        // of the address), then use four flags to indicate which bytes within
        // that region you actually want.
        let b = match address & 0b11 {
            0 => 0b0011,
            2 => 0b1100,
            _ => panic!("Address must be 2-byte aligned"),
        };
        ((address & 0b1111111100) << 4) | (b << 2)
    }

    pub fn read(&self, r: Register) -> Result<u16, SpiError> {
        let cmd = Self::pack_addr(r as u16).to_be_bytes();
        let mut response = [0; 4];

        self.spi.exchange(&cmd, &mut response)?;
        let v = u16::from_le_bytes(response[2..].try_into().unwrap());
        ringbuf_entry!(Trace::Read(r, v));

        Ok(v)
    }

    pub fn write(&self, r: Register, v: u16) -> Result<(), SpiError> {
        // Yes, the address is big-endian while the data is little-endian.
        //
        // I don't make the rules.
        let mut request: [u8; 4] = [0; 4];
        request[..2].copy_from_slice(&Self::pack_addr(r as u16).to_be_bytes());
        request[2..].copy_from_slice(&v.to_le_bytes());
        request[0] |= 0x80; // Set MSB to indicate write.

        ringbuf_entry!(Trace::Write(r, v));
        self.spi.write(&request[..])?;
        Ok(())
    }

    pub fn write_masked(
        &self,
        r: Register,
        v: u16,
        mask: u16,
    ) -> Result<(), SpiError> {
        let v = (self.read(r)? & !mask) | (v & mask);
        self.write(r, v)
    }

    pub fn enabled(&self) -> Result<bool, SpiError> {
        Ok(self.read(Register::CIDER)? & 0x1 != 0)
    }

    pub fn enable(&self) -> Result<(), SpiError> {
        self.write(Register::CIDER, 1)
    }

    pub fn disable(&self) -> Result<(), SpiError> {
        self.write(Register::CIDER, 0)
    }

    /// Reads a management information base (MIB) counter
    ///
    /// `port` must be 1 or 2 to select the relevant port; otherwise, this
    /// function will panic.
    pub fn read_mib_counter(
        &self,
        port: u8,
        offset: MIBOffset,
    ) -> Result<MIBCounter, SpiError> {
        let b = match port {
            1 => 0x0,
            2 => 0x20,
            _ => panic!("Invalid port {}", port),
        };
        // Request counter with given offset.
        self.write(
            Register::IACR,
            (1 << 12) |        // Read
            (0b11 << 10) |     // MIB counter
            offset as u16 + b, // Offset
        )?;

        // Read counter data.
        let hi = self.read(Register::IADR5)?;
        let lo = self.read(Register::IADR4)?;
        let value = u32::from(hi) << 16 | u32::from(lo);

        // Determine state of the counter, see p. 184 of datasheet.
        let invalid = ((1 << 30) & value) != 0;
        let overflow = ((1 << 31) & value) != 0;
        let value: u32 = value & 0x3fffffff;

        if invalid {
            Ok(MIBCounter::Invalid)
        } else if overflow {
            Ok(MIBCounter::CountOverflow(value))
        } else {
            Ok(MIBCounter::Count(value))
        }
    }

    /// Configures the KSZ8463 switch in 100BASE-FX mode.
    pub fn configure(&self, sys: &Sys) {
        use sys_api::*;
        sys.gpio_reset(self.nrst).unwrap();
        sys.gpio_configure_output(
            self.nrst,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        )
        .unwrap();

        // Toggle the reset line
        sleep_for(10); // Reset must be held low for 10 ms after power up
        sys.gpio_set(self.nrst).unwrap();

        // The datasheet recommends a particular combination of diodes and
        // capacitors which dramatically slow down the rise of the reset
        // line, meaning you have to wait for extra long here.
        //
        // Otherwise, the minimum wait time is 1 µs, so 1 ms is fine.
        sleep_for(match self.reset_speed {
            ResetSpeed::Slow => 150,
            ResetSpeed::Normal => 1,
        });

        let id = self.read(Register::CIDER).unwrap();
        assert_eq!(id & !1, 0x8452);
        ringbuf_entry!(Trace::Id(id));

        // Configure for 100BASE-FX operation
        self.write_masked(Register::CFGR, 0x0, 0xc0).unwrap();
        self.write_masked(Register::DSP_CNTRL_6, 0, 0x2000).unwrap();

        // Enable port 1 near-end loopback (XXX delete this before connecting
        // to the rest of the management network)
        self.write_masked(Register::P1PHYCTRL, 1 << 1, 1 << 1)
            .unwrap();

        self.enable().unwrap();
    }
}
