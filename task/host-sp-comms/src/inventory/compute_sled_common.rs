// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_i2c_api::I2cDevice;
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::at24csw080::{At24Csw080, Error as EepromError};
use drv_oxide_vpd::VpdError;
use host_sp_messages::{InventoryData, InventoryDataResult};
use oxide_barcode::{OxideIdentity, VpdIdentity};

impl crate::ServerImpl {
    /// Reads the 128-bit unique ID from an AT24CSW080 EEPROM
    pub(crate) fn read_at24csw080_id(&mut self, sequence: u64, dev: I2cDevice) {
        *self.scratch = InventoryData::At24csw08xSerial([0u8; 16]);
        let name = dev.component_id().as_bytes();
        let eeprom = At24Csw080::new(dev);
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::At24csw08xSerial(id) = self.scratch else {
                unreachable!();
            };
            for (i, b) in id.iter_mut().enumerate() {
                *b = eeprom.read_security_register_byte(i as u8).map_err(
                    |e| match e {
                        EepromError::I2cError(ResponseCode::NoDevice) => {
                            InventoryDataResult::DeviceAbsent
                        }
                        _ => InventoryDataResult::DeviceFailed,
                    },
                )?;
            }
            Ok(self.scratch)
        });
    }

    /// Reads the "BARC" value from a TLV-C blob in an AT24CSW080 EEPROM
    ///
    /// On success, packs the barcode into `self.tx_buf`; on failure, return an
    /// error (`DeviceAbsent` if we saw `NoDevice`, or `DeviceFailed` on all
    /// other errors).
    pub(crate) fn read_eeprom_barcode(
        &mut self,
        sequence: u64,
        dev: I2cDevice,
    ) {
        let mut buf = [0u8; crate::bsp::MAX_COMPONENT_ID_LEN + 3];
        let name = {
            // Append "/ID" to the component's refdes path.
            let dev_id = dev.component_id().as_bytes();
            buf[0..dev_id.len()].copy_from_slice(dev_id);
            let spliced_len = dev_id.len() + 3;
            buf[dev_id.len()..spliced_len].copy_from_slice(b"/ID");
            &buf[..spliced_len]
        };
        let barcode_buf = &mut self.barcode_buf[..];

        *self.scratch = InventoryData::VpdIdentity(Default::default());
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::VpdIdentity(identity) = self.scratch else {
                unreachable!();
            };
            *identity = read_one_barcode::<OxideIdentity>(
                dev,
                &[(*b"BARC", 0)],
                barcode_buf,
            )?
            .into();
            Ok(self.scratch)
        })
    }

    /// Reads the fan EEPROM barcode values into a `FANTRAYv1` IPCC message.
    ///
    /// The fan EEPROM includes nested barcodes:
    /// - The top-level `BARC`, for the assembly
    /// - A nested value `SASY`, which contains four more `BARC` values for each
    ///   individual fan
    ///
    /// On success, packs the barcode into `self.tx_buf`; on failure, return an
    /// error (`DeviceAbsent` if we saw `NoDevice`, or `DeviceFailed` on all
    /// other errors).
    ///
    /// The V1 fan identity message represents VPD barcodes as the parsed
    /// `Identity` struct. This struct can only represent Oxide (OXV1 or OXV2)
    /// barcodes, and cannot represent MPN1-formatted barcodes. The
    /// `FanIdentityV2` message supports both barcode formats, but older host
    /// software may not be able to comprehend it. When encountering
    /// MPN1-formatted barcodes, this method will return an empty `Identity`
    /// struct (all zeros).
    pub(crate) fn read_fan_barcodes_v1(
        &mut self,
        sequence: u64,
        dev: I2cDevice,
    ) {
        let mut buf = [0u8; crate::bsp::MAX_COMPONENT_ID_LEN + 3];
        let name = munge_fantray_refdes(dev.component_id(), &mut buf);
        let barcode_buf = &mut self.barcode_buf[..];

        *self.scratch = InventoryData::FanIdentityV1 {
            identity: Default::default(),
            vpd_identity: Default::default(),
            fans: Default::default(),
        };
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::FanIdentityV1 {
                identity,
                vpd_identity,
                fans,
            } = self.scratch
            else {
                unreachable!();
            };
            *identity = read_one_barcode::<OxideIdentity>(
                dev,
                &[(*b"BARC", 0)],
                barcode_buf,
            )?
            .into();
            *vpd_identity = read_one_barcode::<OxideIdentity>(
                dev,
                &[(*b"SASY", 0), (*b"BARC", 0)],
                barcode_buf,
            )?
            .into();
            read_fan_barcodes(dev, fans, barcode_buf)?;
            Ok(self.scratch)
        });
    }

    /// Reads the fan EEPROM barcode values into a `FANTRAYv2` IPCC message.
    ///
    /// The fan EEPROM includes nested barcodes:
    /// - The top-level `BARC`, for the assembly
    /// - A nested value `SASY`, which contains four more `BARC` values for each
    ///   individual fan
    ///
    /// On success, packs the barcode into `self.tx_buf`; on failure, return an
    /// error (`DeviceAbsent` if we saw `NoDevice`, or `DeviceFailed` on all
    /// other errors).
    ///
    /// Unlike the `read_fan_barcodes_v1` method, this method supports both
    /// Oxide (OXV1 or OXV2) and MPN1-formatted barcodes.
    pub(crate) fn read_fan_barcodes_v2(
        &mut self,
        sequence: u64,
        dev: I2cDevice,
    ) {
        let mut buf = [0u8; crate::bsp::MAX_COMPONENT_ID_LEN + 3];
        let name = munge_fantray_refdes(dev.component_id(), &mut buf);
        let barcode_buf = &mut self.barcode_buf[..];

        *self.scratch = InventoryData::FanIdentityV2 {
            identity: Default::default(),
            vpd_identity: Default::default(),
            fans: Default::default(),
        };
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::FanIdentityV2 {
                identity,
                vpd_identity,
                fans,
            } = self.scratch
            else {
                unreachable!();
            };
            *identity = read_one_barcode::<OxideIdentity>(
                dev,
                &[(*b"BARC", 0)],
                barcode_buf,
            )?
            .into();
            *vpd_identity = read_one_barcode::<OxideIdentity>(
                dev,
                &[(*b"SASY", 0), (*b"BARC", 0)],
                barcode_buf,
            )?
            .into();
            read_fan_barcodes(dev, fans, barcode_buf)?;

            Ok(self.scratch)
        });
    }
}

fn read_fan_barcodes<T>(
    dev: I2cDevice,
    fans: &mut [T; 3],
    barcode_buf: &mut [u8],
) -> Result<(), InventoryDataResult>
where
    T: TryFrom<oxide_barcode::VpdIdentity>,
{
    let [ref mut fan0, ref mut fan1, ref mut fan2] = fans;

    if let Ok(id) = read_one_barcode::<VpdIdentity>(
        dev,
        &[(*b"SASY", 0), (*b"BARC", 1)],
        barcode_buf,
    )? // If reading from the EEPROM fails, return an error
    .try_into()
    // ...but if the identity isn't formatted as something that can be converted
    // into a `T`, just leave it blank
    {
        *fan0 = id;
    }

    if let Ok(id) = read_one_barcode::<VpdIdentity>(
        dev,
        &[(*b"SASY", 0), (*b"BARC", 2)],
        barcode_buf,
    )?
    .try_into()
    {
        *fan1 = id;
    }

    if let Ok(id) = read_one_barcode::<VpdIdentity>(
        dev,
        &[(*b"SASY", 0), (*b"BARC", 3)],
        barcode_buf,
    )?
    .try_into()
    {
        *fan2 = id;
    }

    Ok(())
}

/// Free function to read a nested barcode, translating errors appropriately
fn read_one_barcode<T>(
    dev: I2cDevice,
    path: &[([u8; 4], usize)],
    barcode_buf: &mut [u8],
) -> Result<T, InventoryDataResult>
where
    T: oxide_barcode::ParseBarcode,
{
    let eeprom = At24Csw080::new(dev);
    match drv_oxide_vpd::read_config_nested_from_into(
        eeprom,
        path,
        &mut barcode_buf[..],
    ) {
        Ok(n) => {
            // extract barcode!
            let identity = T::parse_barcode(&barcode_buf[..n])
                .map_err(|_| InventoryDataResult::DeviceFailed)?;
            Ok(identity)
        }
        Err(
            VpdError::ErrorOnBegin(err)
            | VpdError::ErrorOnRead(err)
            | VpdError::ErrorOnNext(err)
            | VpdError::InvalidChecksum(err),
        ) if err
            == tlvc::TlvcReadError::User(EepromError::I2cError(
                ResponseCode::NoDevice,
            )) =>
        {
            Err(InventoryDataResult::DeviceAbsent)
        }
        Err(..) => Err(InventoryDataResult::DeviceFailed),
    }
}

fn munge_fantray_refdes<'buf>(
    dev_id: &str,
    buf: &'buf mut [u8; crate::bsp::MAX_COMPONENT_ID_LEN + 3],
) -> &'buf [u8] {
    let dev_id = dev_id.as_bytes();
    buf[0..dev_id.len()].copy_from_slice(dev_id);
    // Okay, so this is a bit wacky: the host system expects us these
    // refdes paths to be in the form `Jxxx/ID` and *not* `Jxxx/Ux/ID`,
    // so we try and find the last segment in the path and clobber it,
    // if there is one. Otherwise, we append a `/ID` at the end --- in
    // practice, that case *shouldn't* ever happen based on the current
    // Gimlet/Cosmo app.tomls, but let's handle it just in case.
    let last_part =
        buf.iter().rposition(|&b| b == b'/').unwrap_or(dev_id.len());
    buf[last_part..last_part + 3].copy_from_slice(b"/ID");
    &buf[..last_part + 3]
}
