// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{inventory::Inventory, update::sp::SpUpdate, Log, MgsMessage};
use core::{cell::RefCell, convert::Infallible};
use drv_sprot_api::SpRot;
use gateway_messages::{
    DiscoverResponse, ImageVersion, PowerState, RotError, RotState, SpError,
    SpPort, SpState,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_control_plane_agent_api::VpdIdentity;
use task_net_api::MacAddress;
use userlib::kipc;

#[cfg(feature = "vpd-identity")]
userlib::task_slot!(I2C, i2c_driver);

/// Provider of MGS handler logic common to all targets (gimlet, sidecar, psc).
pub(crate) struct MgsCommon {
    reset_requested: bool,
    inventory: Inventory,
    identity: RefCell<Option<VpdIdentity>>,
    base_mac_address: MacAddress,
}

impl MgsCommon {
    pub(crate) fn claim_static_resources(base_mac_address: MacAddress) -> Self {
        Self {
            reset_requested: false,
            inventory: Inventory::new(),
            identity: RefCell::new(None),
            base_mac_address,
        }
    }

    pub(crate) fn discover(
        &mut self,
        port: SpPort,
    ) -> Result<DiscoverResponse, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::Discovery));
        Ok(DiscoverResponse { sp_port: port })
    }

    pub(crate) fn identity(&self) -> VpdIdentity {
        if let Some(identity) = *self.identity.borrow() {
            return identity;
        }

        let id = identity();
        let mut cached = self.identity.borrow_mut();
        *cached = Some(id);
        id
    }

    pub(crate) fn sp_state(
        &mut self,
        update: &SpUpdate,
        power_state: PowerState,
    ) -> Result<SpState, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SpState));

        let id = self.identity();

        let mut state = SpState {
            serial_number: [0; 32],
            model: [0; 32],
            revision: id.revision,
            hubris_archive_id: kipc::read_image_id().to_le_bytes(),
            base_mac_address: self.base_mac_address.0,
            version: update.current_version(),
            power_state,
            rot: rot_state(update.sprot_task()),
        };

        let n = usize::min(state.serial_number.len(), id.serial.len());
        state.serial_number[..n].copy_from_slice(&id.serial);

        let n = usize::min(state.model.len(), id.part_number.len());
        state.model[..n].copy_from_slice(&id.part_number);

        Ok(state)
    }

    pub(crate) fn reset_prepare(&mut self) -> Result<(), SpError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ResetPrepare));
        self.reset_requested = true;
        Ok(())
    }

    pub(crate) fn reset_trigger(&mut self) -> Result<Infallible, SpError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        if !self.reset_requested {
            return Err(SpError::ResetTriggerWithoutPrepare);
        }

        let jefe = task_jefe_api::Jefe::from(crate::JEFE.get_task_id());
        jefe.request_reset();

        // If `request_reset()` returns, something has gone very wrong.
        panic!()
    }

    #[inline(always)]
    pub(crate) fn inventory(&self) -> &Inventory {
        &self.inventory
    }
}

fn rot_state(sprot: &SpRot) -> Result<RotState, RotError> {
    let status = sprot.status().map_err(SprotErrorConvert)?;
    Ok(RotState {
        version: ImageVersion {
            version: status.version,
            epoch: status.epoch,
        },
        messages_received: status.rx_received,
        invalid_messages_received: status.rx_invalid,
        incomplete_transmissions: status.tx_incomplete,
        rx_fifo_overrun: status.rx_overrun,
        tx_fifo_underrun: status.tx_underrun,
    })
}

pub(crate) struct SprotErrorConvert(pub drv_sprot_api::SprotError);

impl From<SprotErrorConvert> for RotError {
    fn from(err: SprotErrorConvert) -> Self {
        RotError::MessageError { code: err.0 as u32 }
    }
}

fn identity() -> VpdIdentity {
    #[cfg(feature = "vpd-identity")]
    fn identity_from_vpd() -> Option<VpdIdentity> {
        use core::{mem, str};
        use zerocopy::{AsBytes, FromBytes};

        #[derive(Debug, Clone, Copy, PartialEq, Eq, AsBytes, FromBytes)]
        #[repr(C, packed)]
        pub struct BarcodeVpd {
            pub version: [u8; 4],
            pub delim0: u8,
            // VPD omits the hyphen 3 bytes into the part number, which we add
            // back into `VpdIdentity` below, hence the "minus 1".
            pub part_number: [u8; VpdIdentity::PART_NUMBER_LEN - 1],
            pub delim1: u8,
            pub revision: [u8; 3],
            pub delim2: u8,
            pub serial: [u8; VpdIdentity::SERIAL_LEN],
        }
        static_assertions::const_assert_eq!(mem::size_of::<BarcodeVpd>(), 31);

        let i2c_task = I2C.get_task_id();
        let barcode: BarcodeVpd =
            drv_local_vpd::read_config(i2c_task, *b"BARC").ok()?;

        // Check expected values of fields, since `barcode` was created
        // via zerocopy (i.e., memcopying a byte array).
        if barcode.delim0 != b':'
            || barcode.delim1 != b':'
            || barcode.delim2 != b':'
        {
            return None;
        }

        // Allow `0` or `O` for the first byte of the version (which isn't
        // part of the identity we return, but tells us the format of the
        // barcode string itself).
        if barcode.version != *b"0XV1" && barcode.version != *b"OXV1" {
            return None;
        }

        let mut identity = VpdIdentity::default();

        // Parse revision into a u32
        identity.revision =
            str::from_utf8(&barcode.revision).ok()?.parse().ok()?;

        // Insert a hyphen 3 characters into the part number (which we know we
        // have room for based on the size of the `part_number` fields)
        identity.part_number[..3].copy_from_slice(&barcode.part_number[..3]);
        identity.part_number[3] = b'-';
        identity.part_number[4..][..barcode.part_number.len() - 3]
            .copy_from_slice(&barcode.part_number[3..]);

        // Copy the serial as-is.
        identity.serial[..barcode.serial.len()]
            .copy_from_slice(&barcode.serial);

        Some(identity)
    }

    #[cfg(feature = "vpd-identity")]
    let id = identity_from_vpd().unwrap_or_default();

    #[cfg(not(feature = "vpd-identity"))]
    let id = VpdIdentity::default();

    id
}
