// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
#![no_std]

use drv_spi_api::{SpiDevice, SpiError, SpiServer};
use ringbuf::*;
use userlib::hl::sleep_for;

mod registers;
pub use registers::{MIBCounter, Register};

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
pub enum Error {
    SpiError(#[count(children)] SpiError),
    WrongChipId(u16),
}

impl From<SpiError> for Error {
    fn from(s: SpiError) -> Self {
        Self::SpiError(s)
    }
}

impl From<idol_runtime::ServerDeath> for Error {
    fn from(_: idol_runtime::ServerDeath) -> Self {
        Self::SpiError(SpiError::TaskRestarted)
    }
}

pub enum VLanMode {
    /// Configure VLAN tags 0x301 and 0x302 for (upstream) ports 1 and 2
    /// respectively.  Allow untagged frames on any port, but drop tagged
    /// frames with an _incorrect_ tag.  Do not use any VLAN tags on port 3
    /// (the downstream port to the SP).
    Optional,

    /// Require VLAN tags on port 3.  Frames tagged with 0x301/0x302 are sent
    /// to ports 1 and 2 respectively; the tag is stripped before egress.
    ///
    /// Reject tagged frames on ingress into ports 1 and 2.
    Mandatory,

    /// This is a tricky one!
    ///
    /// Ports 1 (to the local Sidecar) and 3 (to the SP) require tagged frames.
    ///
    /// Port 1 is associated with VLANs 0x12C, 0x12D, and 0x130, corresponding
    /// to the two tech ports and the Tofino respectively.  These sources are
    /// all local to the Sidecar itself, and we want to distinguish between
    /// them.
    ///
    /// Port 2 requires untagged frames, which are assigned to VID 0x302.  This
    /// is the connection to the rest of the management network, which is an
    /// untagged backplane.
    ///
    /// Port 3 requires tagged frames, which can be any of the above VIDs
    /// (because they're all heading to the SP)
    Sidecar,

    /// Don't do any configuration of the VLANs.
    Off,
}

pub enum Mode {
    /// 10/100BASE-TX mode
    Copper,
    /// 100BASE-FX mode
    Fiber,
}

////////////////////////////////////////////////////////////////////////////////
// NOTE: these VLAN VIDs should match the configuration in the TOML file

/// VLAN for packets arriving on tech port 1
const TP1_VID: u16 = 0x12C;

/// VLAN for packets arriving on tech port 2
const TP2_VID: u16 = 0x12D;

/// VLAN for packets arriving from the Tofino
const TOFINO_VID: u16 = 0x130;

/// VLAN for packets arriving from the local Sidecar's network
///
/// This is mutually exclusive with [`TP1_VID`] / [`TP2_VID`] / [`TOFINO_VID`];
/// depending on how the VSC7448 is configured.
const LOCAL_SIDECAR_VID: u16 = 0x301;

/// VLAN for packets arriving from the peer Sidecar's network
const PEER_SIDECAR_VID: u16 = 0x302;

/// VID used to drop packets
///
/// If an untagged packet arrives on a port that expects tagged packets, it is
/// classified to this VLAN then dropped.
const INVALID_VID: u16 = 0x3FF;

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, Eq, PartialEq)]
enum Trace {
    None,
    Read(Register, u16),
    Write(Register, u16),
    Id(u16),
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

/// Data from a management information base (MIB) counter on the chip,
/// used to monitor port activity for network management.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MIBCounterValue {
    None,
    Count(u32),
    CountOverflow(u32),
}

impl Default for MIBCounterValue {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SourcePort {
    Port1,
    Port2,
    Port3,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct KszRawMacTableEntry {
    /// Number of valid entries in the table
    pub count: u32,

    /// Two-bit counter for internal aging
    timestamp: u8,

    /// Source port where the FID + MAC is learned
    pub source: SourcePort,

    /// Filter ID
    fid: u8,

    /// MAC address from the table
    pub addr: [u8; 6],
}

////////////////////////////////////////////////////////////////////////////////

pub struct Ksz8463<S: SpiServer> {
    spi: SpiDevice<S>,
}

/// KSZ port with a PHY
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum KszPhyPort {
    One,
    Two,
}

/// Any KSZ port
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum KszPort {
    One,
    Two,
    Three,
}

impl From<KszPhyPort> for KszPort {
    fn from(p: KszPhyPort) -> KszPort {
        match p {
            KszPhyPort::One => KszPort::One,
            KszPhyPort::Two => KszPort::Two,
        }
    }
}

impl<S: SpiServer> Ksz8463<S> {
    pub fn new(spi: SpiDevice<S>) -> Self {
        Self { spi }
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

    pub fn read(&self, r: Register) -> Result<u16, Error> {
        let cmd = Self::pack_addr(r as u16).to_be_bytes();
        let mut response = [0; 4];

        self.spi.exchange(&cmd, &mut response)?;
        let v = u16::from_le_bytes(response[2..].try_into().unwrap());
        ringbuf_entry!(Trace::Read(r, v));

        Ok(v)
    }

    pub fn write(&self, r: Register, v: u16) -> Result<(), Error> {
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

    /// Performs a read-modify-write operation on a PHY register
    #[inline(always)]
    pub fn modify<F>(&self, reg: Register, f: F) -> Result<(), Error>
    where
        F: Fn(&mut u16),
    {
        let mut data = self.read(reg)?;
        f(&mut data);
        self.write(reg, data)
    }

    pub fn enabled(&self) -> Result<bool, Error> {
        Ok(self.read(Register::CIDER)? & 0x1 != 0)
    }

    pub fn enable(&self) -> Result<(), Error> {
        self.write(Register::CIDER, 1)
    }

    pub fn disable(&self) -> Result<(), Error> {
        self.write(Register::CIDER, 0)
    }

    /// Reads a management information base (MIB) counter
    ///
    /// `port` must be 1 or 2 to select the relevant port; otherwise, this
    /// function will panic.
    pub fn read_mib_counter(
        &self,
        port: KszPort,
        offset: MIBCounter,
    ) -> Result<MIBCounterValue, Error> {
        let b = match port {
            KszPort::One => 0x0,
            KszPort::Two => 0x20,
            KszPort::Three => 0x40,
        };
        // Request counter with given offset.
        self.write(
            Register::IACR,
            (1 << 12) |          // Read
            (0b11 << 10) |       // MIB counter
            (offset as u16 + b), // Offset
        )?;

        // Read counter data, looping until the 'valid' bit is 1
        let hi = loop {
            let hi = self.read(Register::IADR5)?;
            if hi & (1 << 14) != 0 {
                break hi;
            }
        };

        let lo = self.read(Register::IADR4)?;
        let value = u32::from(hi) << 16 | u32::from(lo);

        // Determine state of the counter, see p. 184 of datasheet.
        let overflow = ((1 << 31) & value) != 0;
        let value: u32 = value & 0x3fffffff;

        if overflow {
            Ok(MIBCounterValue::CountOverflow(value))
        } else {
            Ok(MIBCounterValue::Count(value))
        }
    }

    /// Reads an entry from the dynamic MAC address table.
    /// `addr` must be < 1024, otherwise this will panic.
    pub fn read_dynamic_mac_table(
        &self,
        addr: u16,
    ) -> Result<Option<KszRawMacTableEntry>, Error> {
        assert!(addr < 1024);
        self.write(Register::IACR, 0x1800 | addr)?;
        // Wait for the "not ready" bit to be cleared
        //
        // The IADR* registers together form a 72-bit value, which is packed
        // into a set of u16 values; we use variables of the form `d_HI_LO`,
        // where `HI` and `LO` are bit ranges in that value.
        //
        // Each `d_*` variable uses all 16 bits *except* `d_71_64`, which only
        // uses the lower 8 bits.
        let d_71_64 = loop {
            let d = self.read(Register::IADR1)?;
            // Check bit 71 to see if the register is ready
            if d & (1 << 7) == 0 {
                break d;
            }
        };
        // This ordering of IADR reads is straight out of the datasheet;
        // heaven forbid they be in a sensible order.
        let d_63_48 = self.read(Register::IADR3)?;
        let d_47_32 = self.read(Register::IADR2)?;
        let d_31_16 = self.read(Register::IADR5)?;
        let d_15_0 = self.read(Register::IADR4)?;

        let empty = (d_71_64 & 4) != 0;
        if empty {
            return Ok(None);
        }

        // Awkwardly stradling the line between two words...
        let count =
            (d_71_64 as u32 & 0b11) << 8 | (d_63_48 as u32 & 0xFF00) >> 8;

        let timestamp = (d_63_48 >> 6) as u8 & 0b11;
        let source = match (d_63_48 >> 4) & 0b11 {
            0 => SourcePort::Port1,
            1 => SourcePort::Port2,
            2 => SourcePort::Port3,
            _ => panic!("Invalid port"),
        };
        let fid = (d_63_48 & 0b1111) as u8;

        let addr = [
            (d_47_32 >> 8) as u8,
            d_47_32 as u8,
            (d_31_16 >> 8) as u8,
            d_31_16 as u8,
            (d_15_0 >> 8) as u8,
            d_15_0 as u8,
        ];

        Ok(Some(KszRawMacTableEntry {
            count: count + 1, // table is non-empty
            timestamp,
            source,
            fid,
            addr,
        }))
    }

    /// Configures an entry in the VLAN table.  There are various constraints
    /// on incoming values:
    /// ```
    ///     table_entry <= 15
    ///     port_mask <= 0b111
    ///     vlan_id <= 4096
    /// ```
    ///
    /// We assume that `table_entry` is the same as the desired FID.
    ///
    /// The function will panic if these constraints are not met.
    fn write_vlan_table(
        &self,
        table_entry: u8,
        port_mask: u8,
        vlan_id: u16,
    ) -> Result<(), Error> {
        assert!(table_entry <= 15);
        assert!(port_mask <= 0b111);
        assert!(vlan_id <= 4096);

        let cmd = vlan_id as u32
            | (u32::from(true) << 19) // valid
            | (u32::from(port_mask) << 16) // ports
            | (u32::from(table_entry) << 12); // FID
        self.write(Register::IADR5, (cmd >> 16) as u16)?;
        self.write(Register::IADR4, cmd as u16)?;
        self.write(Register::IACR, 0x400 | u16::from(table_entry))
    }

    /// Disables an entry in the VLAN table.  This is particularly important
    /// to disable VLAN 1, which otherwise is allowed on all ports.
    fn disable_vlan(&self, table_entry: u8) -> Result<(), Error> {
        self.write(Register::IADR5, 0)?;
        self.write(Register::IADR4, 0)?;
        self.write(Register::IACR, 0x400 | u16::from(table_entry))
    }

    /// Configure the given port as untagged
    ///
    /// An untagged port will drop tagged ingress packets (to prevent
    /// shenanigans) and will strip tags on egress.
    fn configure_untagged(&self, port: KszPort, vid: u16) -> Result<(), Error> {
        self.modify(Register::PxCR1(port), |r| {
            *r |= 1 << 9; // Drop tagged ingress packets
            *r |= 1 << 1; // Remove tags on egress
        })?;
        self.write(Register::PxVIDCR(port), vid)?;

        Ok(())
    }

    /// Configures the given port as tagged
    ///
    /// Tagged ports will drop packets with invalid tags
    fn configure_tagged(&self, port: KszPort) -> Result<(), Error> {
        // Insert tags before egress
        self.modify(Register::PxCR1(port), |r| *r |= 1 << 2)?;

        // Untagged packets arriving on this port will be assigned to an invalid
        // VID, which includes no ports.
        self.write(Register::PxVIDCR(port), INVALID_VID)?;

        // This will cause them to drop packets that have invalid
        // tags (and untagged frames will be assigned 0x3FF then
        // unceremoniously dropped).
        self.modify(Register::PxCR2(port), |r| *r |= 1 << 14)?;

        Ok(())
    }

    /// Configures the KSZ8463 switch in 100BASE-FX mode.
    pub fn configure(
        &self,
        mode: Mode,
        vlan_mode: VLanMode,
    ) -> Result<(), Error> {
        let id = self.read(Register::CIDER)? & !1;
        ringbuf_entry!(Trace::Id(id));
        if id != 0x8452 {
            return Err(Error::WrongChipId(id));
        }

        // Do a full software reset of the chip to put registers into
        // a known state.
        self.write(Register::GRR, 1)?;
        sleep_for(10);
        self.write(Register::GRR, 0)?;

        match mode {
            Mode::Fiber => {
                // Configure for 100BASE-FX operation
                self.modify(Register::CFGR, |r| *r &= !0xc0)?;
                self.modify(Register::DSP_CNTRL_6, |r| *r &= !0x2000)?;
            }
            Mode::Copper => (), // No changes from defaults
        }

        match vlan_mode {
            // In `VLanMode::Optional`, we allow tags on the upstream ports,
            // but strip them before frames are delivered downstream.  This
            // lets us test the VLAN before the SP netstack supports tags.
            VLanMode::Optional => {
                // Configure VLAN table for the device:
                // - VLAN 0 has tag 0x301, and contains ports 1 and 3
                // - VLAN 1 has tag 0x302, and contains ports 2 and 3
                // - VLAN 2 has tag 0x3FF, and contains all ports
                //
                // This uses slots 0-2 in the table, and FID 0-2 (same as slot),
                // then disables the remaining slots in the VLAN table.
                self.write_vlan_table(0, 0b101, LOCAL_SIDECAR_VID)?;
                self.write_vlan_table(1, 0b110, PEER_SIDECAR_VID)?;
                self.write_vlan_table(2, 0b111, INVALID_VID)?;
                for i in 3..16 {
                    self.disable_vlan(i)?;
                }

                // Assign default VLAN tags to each port
                self.write(Register::P1VIDCR, LOCAL_SIDECAR_VID)?;
                self.write(Register::P2VIDCR, PEER_SIDECAR_VID)?;
                self.write(Register::P3VIDCR, INVALID_VID)?;

                // Enable ingress VLAN filtering on upstream ports
                for i in [KszPort::One, KszPort::Two] {
                    self.modify(Register::PxCR2(i), |r| *r |= 1 << 14)?;
                }

                // Enable tag removal on the downstream port
                self.modify(Register::P3CR1, |r| *r |= 1 << 1)?;
            }
            // In `VLanMode::Mandatory`, we expect untagged frames on Port 1/2
            // and tagged frames on Port 3. Untagged frames arriving on Port 3
            // are assigned to VLAN 0x3FF, which drops them.
            VLanMode::Mandatory => {
                // Configure VLAN table for the device:
                // - VLAN 0 has tag 0x301, and contains ports 1 and 3
                // - VLAN 1 has tag 0x302, and contains ports 2 and 3
                // - VLAN 2 has tag 0x3FF, and contains no ports
                //
                // This uses slots 0-2 in the table, and FID 0-2 (same as
                // slot); all other VLANs are disabled (in particular, VLAN
                // with VID 1, which by default includes all ports).
                self.write_vlan_table(0, 0b101, LOCAL_SIDECAR_VID)?;
                self.write_vlan_table(1, 0b110, PEER_SIDECAR_VID)?;
                self.write_vlan_table(2, 0b000, INVALID_VID)?;
                for i in 3..16 {
                    self.disable_vlan(i)?;
                }

                // Mark both upstream ports as untagged and configure the VLAN
                // ID that is assigned to them on ingress.
                self.configure_untagged(KszPort::One, LOCAL_SIDECAR_VID)?;
                self.configure_untagged(KszPort::Two, PEER_SIDECAR_VID)?;

                // Mark the downstream port as tagged (between the KSZ8463 and
                // the SP), because we need to know from whence a packet came
                self.configure_tagged(KszPort::Three)?;

                // There's a secret bonus register which _actually_ enables
                // PVID tagging, despite not being mentioned in the VLAN
                // tagging section of the datasheet.  We enable tagging when
                // frames come from Ports 1 and 2 and go to Port 3.
                self.write(Register::SGCR9, (1 << 3) | (1 << 1))?;
            }

            // In `VLanMode::Sidecar`, we expect tagged frames on Ports 1/3
            // and untagged frames on Port 2. Untagged frames arriving on Port
            // 1/3 are assigned to VLAN 0x3FF, which drops them.
            VLanMode::Sidecar => {
                // Configure VLAN table for the device:
                // - VLAN 0 has tag 0x12C, and contains ports 1 and 3 (tp1)
                // - VLAN 1 has tag 0x12D, and contains ports 1 and 3 (tp2)
                // - VLAN 2 has tag 0x130, and contains ports 1 and 3 (Tofino)
                // - VLAN 3 has tag 0x302, and contains ports 2 and 3
                // - VLAN 4 has tag 0x3FF, and contains no ports
                //
                // This uses slots 0-4 in the table, and FID 0-4 (same as
                // slot); all other VLANs are disabled (in particular, VLAN
                // with VID 1, which by default includes all ports).

                self.write_vlan_table(0, 0b101, TP1_VID)?;
                self.write_vlan_table(1, 0b101, TP2_VID)?;
                self.write_vlan_table(2, 0b101, TOFINO_VID)?;
                self.write_vlan_table(3, 0b110, PEER_SIDECAR_VID)?;
                self.write_vlan_table(4, 0b000, INVALID_VID)?;
                for i in 5..16 {
                    self.disable_vlan(i)?;
                }

                // Configure port 2 as untagged and give it a VID
                self.configure_untagged(KszPort::Two, PEER_SIDECAR_VID)?;

                // Configure ports 1 and 3 as tagged
                for p in [KszPort::One, KszPort::Three] {
                    self.configure_tagged(p)?;
                }

                // There's a secret bonus register which _actually_ enables
                // PVID tagging, despite not being mentioned in the VLAN
                // tagging section of the datasheet.  We enable tagging when
                // frames come from Port 2 and go to Port 3.
                self.write(Register::SGCR9, 1 << 3)?;
            }

            VLanMode::Off => (),
        }

        // Enable 802.1Q VLAN mode.  This must happen after the VLAN tables
        // are configured.
        match vlan_mode {
            VLanMode::Optional | VLanMode::Mandatory | VLanMode::Sidecar => {
                self.modify(Register::SGCR2, |r| {
                    *r |= 1 << 15;
                })?
            }
            VLanMode::Off => (),
        }

        self.enable()
    }
}
