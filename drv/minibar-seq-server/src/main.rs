// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Minibar sequencing process.

#![no_std]
#![no_main]

use drv_auxflash_api::AuxFlash;
use drv_fpga_api::{
    await_fpga_ready, BitstreamType, DeviceState, Fpga, FpgaError,
    FpgaUserDesign, FpgaUserDesignIdent, WriteOp,
};
use drv_minibar_seq_api::{
    Addr, MinibarSeqError, Reg, MINIBAR_BITSTREAM_CHECKSUM,
};
use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};

use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::{
    sys_get_timer, sys_set_timer, task_slot, RecvMessage, UnwrapLite,
};

task_slot!(I2C, i2c_driver);
task_slot!(FPGA, ecp5);
task_slot!(AUXFLASH, auxflash);
task_slot!(PACKRAT, packrat);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Init,
    FpgaInit,
    LoadingFpgaBitstream,
    SettingShortChecksum,
    FpgaBitstreamError(FpgaError),
    SkipLoadingBitstream,
    ControllerId(u32),
    InvalidControllerId(u32),
    ControllerChecksum(u32),
    ExpectedControllerChecksum(u32),
    ControllerVersion(u32),
    ControllerSha(u32),
    FpgaInitComplete,
    FpgaWriteError(FpgaError),
    PcieRefclkPdCleared,
    DeviceState(DeviceState),
}
ringbuf!(Trace, 32, Trace::None);

const TIMER_INTERVAL: u64 = 1000;

struct ServerImpl {
    fpga_config: Fpga,
    fpga_user: FpgaUserDesign,
}

impl ServerImpl {
    /// Returns the expected (short) checksum, which simply a prefix of the full
    /// SHA3-256 hash of the bitstream.
    pub fn short_bitstream_checksum() -> u32 {
        u32::from_le_bytes(MINIBAR_BITSTREAM_CHECKSUM[..4].try_into().unwrap())
    }

    /// Load the FPGA bitstream.
    pub fn load_bitstream(
        &mut self,
        auxflash: userlib::TaskId,
    ) -> Result<(), FpgaError> {
        let mut auxflash = AuxFlash::from(auxflash);
        let blob = auxflash
            .get_blob_by_tag(*b"FPGA")
            .map_err(|_| FpgaError::AuxMissingBlob)?;
        drv_fpga_api::load_bitstream_from_auxflash(
            &mut self.fpga_config,
            &mut auxflash,
            blob,
            BitstreamType::Compressed,
            MINIBAR_BITSTREAM_CHECKSUM,
        )
    }

    /// Set the checksum write-once registers to the expected checksum.
    ///
    /// In concert with `short_bitstream_checksum_valid`, this will detect when
    /// the bitstream of an already running mainboard controller does
    /// (potentially) not match the APIs used to build Hubris.
    pub fn set_short_bitstream_checksum(&self) -> Result<(), FpgaError> {
        self.fpga_user.write(
            WriteOp::Write,
            Addr::CS0,
            ServerImpl::short_bitstream_checksum().to_be(),
        )
    }

    /// Check whether the Ident checksum matches the short bitstream checksum.
    ///
    /// This allows us to detect cases where the Hubris image has been updated
    /// while the FPGA remained powered: if the checksum of the FPGA bitstream
    /// in the new Hubris image has changed it will no longer match the Ident.
    pub fn short_bitstream_checksum_valid(
        &self,
        ident: &FpgaUserDesignIdent,
    ) -> bool {
        ident.checksum.get() == ServerImpl::short_bitstream_checksum()
    }
}

impl idl::InOrderSequencerImpl for ServerImpl {
    fn controller_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<MinibarSeqError>> {
        let state = self
            .fpga_config
            .state()
            .map_err(MinibarSeqError::from)
            .map_err(RequestError::from)?;

        ringbuf_entry!(Trace::DeviceState(state));

        Ok(state == DeviceState::RunningUserDesign)
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, bits: userlib::NotificationBits) {
        if !bits.has_timer_fired(notifications::TIMER_MASK) {
            return;
        }
        let start = sys_get_timer().now;
        let finish = sys_get_timer().now;

        // We now know when we were notified and when any work was completed.
        // Note that the assumption here is that `start` < `finish` and that
        // this won't hold if the system time rolls over. But, the system timer
        // is a u64, with each bit representing a ms, so in practice this should
        // be fine. Anyway, armed with this information, find the next deadline
        // some multiple of `TIMER_INTERVAL` in the future.

        // The timer is monotonic, so finish >= start, so we use wrapping_add
        // here to avoid an overflow check that the compiler conservatively
        // inserts.
        let delta = finish.wrapping_sub(start);
        let next_deadline = finish + TIMER_INTERVAL - (delta % TIMER_INTERVAL);

        sys_set_timer(Some(next_deadline), notifications::TIMER_MASK);
    }
}

#[export_name = "main"]
fn main() -> ! {
    pub const DEVICE_INDEX: u8 = 0;
    pub const EXPECTED_ID: u32 = 0x01de_5bae;

    ringbuf_entry!(Trace::Init);
    let i2c_task = I2C.get_task_id();
    let fpga_config = Fpga::new(FPGA.get_task_id(), DEVICE_INDEX);
    let fpga_user = FpgaUserDesign::new(FPGA.get_task_id(), DEVICE_INDEX);
    let auxflash_task = AUXFLASH.get_task_id();

    let mut server = ServerImpl {
        fpga_config,
        fpga_user,
    };

    ringbuf_entry!(Trace::FpgaInit);

    // Check to see if the FPGA has already been configured
    match await_fpga_ready(&mut server.fpga_config, 25)
        .unwrap_or(DeviceState::Unknown)
    {
        // FPGA is unconfigured, lets load it
        DeviceState::AwaitingBitstream => {
            ringbuf_entry!(Trace::LoadingFpgaBitstream);

            // Attempt to load the bitsream
            match server.load_bitstream(auxflash_task) {
                Ok(()) => {
                    // It worked! lets go write the checksum info
                    ringbuf_entry!(Trace::SettingShortChecksum);
                    server.set_short_bitstream_checksum().unwrap_lite();
                }
                Err(e) => {
                    ringbuf_entry!(Trace::FpgaBitstreamError(e));
                    // If this is an auxflash error indicating that we can't
                    // find the target blob, then it's possible that data isn't
                    // present (i.e. this is an initial boot at the factory). To
                    // prevent this task from spinning too hard, we add a brief
                    // delay before resetting.
                    //
                    // Note that other auxflash errors (e.g. a failed read) will
                    // reset immediately, matching existing behavior on a failed
                    // FPGA reset.
                    if matches!(e, FpgaError::AuxMissingBlob) {
                        userlib::hl::sleep_for(100);
                    }
                    panic!();
                }
            }
        }

        // FPGA is configured, so lets not reload it just yet since we may not need to
        DeviceState::RunningUserDesign => {
            ringbuf_entry!(Trace::SkipLoadingBitstream);
        }
        _ => panic!(),
    }

    // Read the design Ident and determine if a bitstream reload is needed.
    let ident: FpgaUserDesignIdent =
        server.fpga_user.read(Addr::ID0).unwrap_lite();

    match ident.id.into() {
        EXPECTED_ID => {
            ringbuf_entry!(Trace::ControllerId(ident.id.into()))
        }
        _ => {
            // The FPGA is running something unexpected. Reset the device and
            // fire the escape thrusters. This will force a bitstream load when
            // the task is restarted.
            ringbuf_entry!(Trace::InvalidControllerId(ident.id.into()));
            server.fpga_config.reset().unwrap_lite();
            panic!()
        }
    }

    ringbuf_entry!(Trace::ControllerChecksum(ident.checksum.into()));

    if !server.short_bitstream_checksum_valid(&ident) {
        ringbuf_entry!(Trace::ExpectedControllerChecksum(
            ServerImpl::short_bitstream_checksum()
        ));

        // The controller does not match the checksum of the
        // bitstream which is expected to run. This means the register map
        // may not match the APIs in this binary so a bitstream reload is
        // required.

        // Reset the FPGA and deploy the parashutes. This will cause the
        // bitstream to be reloaded when the task is restarted.
        server.fpga_config.reset().unwrap_lite();
        panic!()
    }

    // The expected version of the controller is running. Log some more details.
    ringbuf_entry!(Trace::ControllerVersion(ident.version.into()));
    ringbuf_entry!(Trace::ControllerSha(ident.sha.into()));
    ringbuf_entry!(Trace::FpgaInitComplete);

    // Populate packrat with our mac address and identity.
    let packrat = Packrat::from(PACKRAT.get_task_id());
    read_vpd_and_load_packrat(&packrat, i2c_task);

    // The FPGA has the default refclk buffer straps set, but will hold the device's power-down pin
    // low. Let's clear that pin so the device will sample its straps and begin operation.
    match server.fpga_user.write(
        WriteOp::BitClear,
        Addr::PCIE_REFCLK_CTRL,
        Reg::PCIE_REFCLK_CTRL::PD,
    ) {
        Ok(_) => ringbuf_entry!(Trace::PcieRefclkPdCleared),
        Err(e) => ringbuf_entry!(Trace::FpgaWriteError(e)),
    };

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    let deadline = sys_get_timer().now;
    sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::MinibarSeqError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
