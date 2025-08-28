// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_i2c_api::I2cDevice;
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::at24csw080::{At24Csw080, Error as EepromError};
use drv_oxide_vpd::VpdError;

use host_sp_messages::{InventoryData, InventoryDataResult};

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
        *self.scratch = InventoryData::VpdIdentity(Default::default());
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::VpdIdentity(identity) = self.scratch else {
                unreachable!();
            };
            *identity = read_one_barcode(dev, &[(*b"BARC", 0)])?.into();
            Ok(self.scratch)
        })
    }

    /// Reads the fan EEPROM barcode values
    ///
    /// The fan EEPROM includes nested barcodes:
    /// - The top-level `BARC`, for the assembly
    /// - A nested value `SASY`, which contains four more `BARC` values for each
    ///   individual fan
    ///
    /// On success, packs the barcode into `self.tx_buf`; on failure, return an
    /// error (`DeviceAbsent` if we saw `NoDevice`, or `DeviceFailed` on all
    /// other errors).
    pub(crate) fn read_fan_barcodes(&mut self, sequence: u64, dev: I2cDevice) {
        let mut buf = [0u8; crate::bsp::MAX_COMPONENT_ID_LEN + 3];
        let name = {
            let dev_id = dev.component_id().as_bytes();
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
        };

        *self.scratch = InventoryData::FanIdentity {
            identity: Default::default(),
            vpd_identity: Default::default(),
            fans: Default::default(),
        };
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::FanIdentity {
                identity,
                vpd_identity,
                fans: [fan0, fan1, fan2],
            } = self.scratch
            else {
                unreachable!();
            };
            *identity = read_one_barcode(dev, &[(*b"BARC", 0)])?.into();
            *vpd_identity =
                read_one_barcode(dev, &[(*b"SASY", 0), (*b"BARC", 0)])?.into();
            *fan0 =
                read_one_barcode(dev, &[(*b"SASY", 0), (*b"BARC", 1)])?.into();
            *fan1 =
                read_one_barcode(dev, &[(*b"SASY", 0), (*b"BARC", 2)])?.into();
            *fan2 =
                read_one_barcode(dev, &[(*b"SASY", 0), (*b"BARC", 3)])?.into();
            Ok(self.scratch)
        })
    }
}

/// Free function to read a nested barcode, translating errors appropriately
fn read_one_barcode(
    dev: I2cDevice,
    path: &[([u8; 4], usize)],
) -> Result<oxide_barcode::VpdIdentity, InventoryDataResult> {
    let eeprom = At24Csw080::new(dev);
    let mut barcode = [0; 32];
    match drv_oxide_vpd::read_config_nested_from_into(
        eeprom,
        path,
        &mut barcode,
    ) {
        Ok(n) => {
            // extract barcode!
            let identity = oxide_barcode::VpdIdentity::parse(&barcode[..n])
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
