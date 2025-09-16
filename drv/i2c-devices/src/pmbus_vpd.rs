// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Abstractions for reading PMBus identity data from any PMBus device.

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
pub enum Error {
    I2c {
        cmd: Cmd,
        #[count(children)]
        err: drv_i2c_api::ResponseCode,
    },
}

#[derive(
    Copy,
    Clone,
    Eq,
    PartialEq,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::Immutable,
    counters::Count,
)]
#[repr(u8)]
pub enum Cmd {
    MfrId = pmbus::CommandCode::MFR_ID as u8,
    MfrModel = pmbus::CommandCode::MFR_MODEL as u8,
    MfrRevision = pmbus::CommandCode::MFR_REVISION as u8,
    MfrSerial = pmbus::CommandCode::MFR_SERIAL as u8,
    MfrLocation = pmbus::CommandCode::MFR_LOCATION as u8,
    MfrDate = pmbus::CommandCode::MFR_LOCATION as u8,
    IcDeviceId = pmbus::CommandCode::IC_DEVICE_ID as u8,
    IcDeviceRev = pmbus::CommandCode::IC_DEVICE_REV as u8,
}

pub trait PmbusVpd {
    const HAS_MFR_SERIAL: bool;
    const HAS_MFR_LOCATION: bool;
    const HAS_MFR_DATE: bool;
    /// If `true`, attempt to read the `IC_DEVICE_ID` and `IC_DEVICE_REV` PMBus
    /// registers.
    const HAS_IC_DEVICE_IDENTITY: bool;

    fn read_pmbus_vpd<'buf>(
        dev: &drv_i2c_api::I2cDevice,
        buf: &'buf mut [u8; PmbusIdentity::MAX_LEN],
    ) -> Result<PmbusIdentity<'buf>, Error> {
        use core::ops::Range;

        fn read(
            dev: &drv_i2c_api::I2cDevice,
            cmd: PmbusVpdCmd,
            buf: &mut [u8],
            curr_off: &mut usize,
        ) -> Result<Range<usize>, PmbusVpdError> {
            let off = *curr_off;
            // PMBus block reads may not be longer than 32 bytes. Clamp this
            // down as `drv_i2c_api` gets mad if it sees a lease of >255B.
            let Some(block) = buf.get_mut(off..off + PmbusIdentity::BLOCK_LEN)
            else {
                return Err(PmbusVpdError::BufferTooSmall { cmd });
            };
            let len = dev
                .read_block(cmd, block)
                .map_err(|err| PmbusVpdError::I2c { cmd, err })?;
            *curr_off += len;
            Ok(off..*curr_off)
        }

        let mut off = 0;
        let mfr_range = read(dev, PmbusVpdCmd::MfrId, buf, &mut off)?;
        let model_range = read(dev, PmbusVpdCmd::MfrModel, buf, &mut off)?;
        let rev_range = read(dev, PmbusVpdCmd::MfrRevision, buf, &mut off)?;
        let serial_range = if Self::HAS_MFR_SERIAL {
            Some(read(dev, PmbusVpdCmd::MfrSerial, buf, &mut off)?)
        } else {
            None
        };
        let location_range = if Self::HAS_MFR_LOCATION {
            Some(read(dev, PmbusVpdCmd::MfrLocation, buf, &mut off)?)
        } else {
            None
        };
        let date_range = if Self::HAS_MFR_DATE {
            Some(read(dev, PmbusVpdCmd::MfrDate, buf, &mut off)?)
        } else {
            None
        };
        let ic_device = if Self::HAS_IC_DEVICE_IDENTITY {
            let id_range = read(dev, PmbusVpdCmd::IcDeviceId, buf, &mut off)?;
            let rev_range = read(dev, PmbusVpdCmd::IcDeviceRev, buf, &mut off)?;
            Some(IcDeviceIdentity {
                id: &buf[id_range],
                rev: &buf[rev_range],
            })
        } else {
            None
        };

        Ok(PmbusIdentity {
            mfr_id: &buf[mfr_range],
            mfr_model: &buf[model_range],
            mfr_revision: &buf[rev_range],
            mfr_location: location_range.map(|r| &buf[r]),
            mfr_date: date_range.map(|r| &buf[r]),
            mfr_serial: serial_range.map(|r| &buf[r]),
            ic_device,
        })
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PmbusIdentity<'buf> {
    /// `MFR_ID` (PMBus operation 0x99)
    pub mfr_id: &'buf [u8],
    /// `MFR_MODEL` (PMBus operation 0x9A)
    pub mfr_model: &'buf [u8],
    /// `MFR_REVISION` (PMBus operation 0x9B)
    pub mfr_revision: &'buf [u8],
    /// `MFR_LOCATION` (PMBus operation 0x9C)
    pub mfr_location: Option<&'buf [u8]>,
    /// `MFR_DATE` (PMBus operation 0x9D)
    pub mfr_date: Option<&'buf [u8]>,
    /// `MFR_SERIAL` (PMBus operation 0x9E)
    pub mfr_serial: Option<&'buf [u8]>,

    pub ic_device: Option<IcDeviceIdentity<'buf>>,
}

pub struct IcDeviceIdentity<'buf> {
    /// `IC_DEVICE_ID` (PMBus operation 0xAD)
    pub id: &'buf [u8],
    /// `IC_DEVICE_REV` (PMBus operation 0xAE)
    pub rev: &'buf [u8],
}

impl<'buf> PmbusIdentity<'buf> {
    /// SMBus block reads may not be longer than 32 bytes.
    const BLOCK_LEN: usize = 32;
    /// Maximum length currently required to read a complete set of VPD
    /// registers from a PMBus device.
    ///
    /// Currently, this is 8 32-byte blocks (one for each register that we may
    /// read). If more values are added in the future, this will need to be
    /// embiggened.
    pub const MAX_LEN: usize = Self::BLOCK_LEN * 8;
}
