// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing FPGA devices.

#![no_std]
#![no_main]

use ringbuf::*;
use userlib::*;
use zerocopy::{byteorder, AsBytes, Unaligned, U16};

use drv_fpga_api::{BitstreamType, DeviceState, FpgaError, WriteOp};
use drv_fpga_devices::{
    ecp5, ecp5::Ecp5, ecp5_spi::Ecp5UsingSpi, Fpga, FpgaBitstream,
    FpgaUserDesign,
};
use drv_spi_api::Spi;
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use idol_runtime::{ClientError, Leased, LenLimit, R, W};

task_slot!(SYS, sys);
task_slot!(SPI, spi_driver);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    DeviceId(u32),
    StartBitstreamLoad(BitstreamType),
    ContinueBitstreamLoad(usize),
    FinishBitstreamLoad(usize),
    Locked(TaskId),
    Released(TaskId),
}
ringbuf!(Trace, 64, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let sys = Sys::from(SYS.get_task_id());
    let configuration_port = Spi::from(SPI.get_task_id()).device(0);
    let user_design = Spi::from(SPI.get_task_id()).device(1);

    cfg_if::cfg_if! {
        if #[cfg(target_board = "sidecar-1")] {
            let driver = Ecp5UsingSpi {
                sys,
                done: sys_api::Port::J.pin(15),
                init_n: sys_api::Port::J.pin(12),
                program_n: sys_api::Port::J.pin(13),
                configuration_port,
                user_design,
                user_design_reset_n: sys_api::Port::J.pin(14),
                user_design_reset_duration: ecp5::USER_DESIGN_RESET_DURATION,
            };
        } else if #[cfg(target_board = "gimletlet-2")] {
            let driver = Ecp5UsingSpi {
                sys,
                done: sys_api::Port::E.pin(15),
                init_n: sys_api::Port::D.pin(12),
                program_n: sys_api::Port::B.pin(10),
                configuration_port,
                user_design,
                user_design_reset_n: sys_api::Port::D.pin(11),
                user_design_reset_duration: ecp5::USER_DESIGN_RESET_DURATION,
            };
        } else {
            compile_error!("Board is not supported by the task/fpga");
        }
    }
    driver.configure_gpio();

    let device = Ecp5::new(driver);
    let mut incoming = [0u8; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        lock_holder: None,
        device: &device,
        buffer: [0u8; 128],
        bitstream_loader: None,
    };

    if let Ok(DeviceState::AwaitingBitstream) = server.device.device_state() {
        ringbuf_entry!(Trace::DeviceId(server.device.device_id().unwrap()));
    }

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

enum BitstreamLoader<'a, Device: Fpga<'a>> {
    Uncompressed(Device::Bitstream, usize),
    Compressed(gnarle::Decompressor, Device::Bitstream, usize),
}

struct ServerImpl<'a, Device: Fpga<'a> + FpgaUserDesign> {
    lock_holder: Option<userlib::TaskId>,
    device: &'a Device,
    buffer: [u8; 128],
    bitstream_loader: Option<BitstreamLoader<'a, Device>>,
}

/// This UserDesignLock is used to ensure atomic read/write operations to the
/// user design port.
///
/// TODO (arjen): This should probably move into the FpgaUserDesign trait, but
/// the semantics are a bit messy. This will do the correct thing for now, until
/// there is a bit more time to consider the semantics of the SPI driver and
/// what is intended to happen when an FPGA API user wants to lock the user
/// design.
struct UserDesignLock<'a, Device: FpgaUserDesign>(&'a Device);

impl<Device: FpgaUserDesign> Drop for UserDesignLock<'_, Device> {
    fn drop(&mut self) {
        self.0.user_design_release().unwrap()
    }
}

impl<'a, Device: Fpga<'a> + FpgaUserDesign> ServerImpl<'a, Device> {
    fn lock_user_design(
        &self,
    ) -> Result<UserDesignLock<'a, Device>, FpgaError> {
        self.device.user_design_lock().map_err(FpgaError::from)?;
        Ok(UserDesignLock(self.device))
    }
}

type RequestError = idol_runtime::RequestError<FpgaError>;
type ReadDataLease = LenLimit<Leased<R, [u8]>, 128>;
type WriteDataLease = LenLimit<Leased<W, [u8]>, 128>;

impl<'a, Device: Fpga<'a> + FpgaUserDesign> idl::InOrderFpgaImpl
    for ServerImpl<'a, Device>
{
    fn recv_source(&self) -> Option<userlib::TaskId> {
        self.lock_holder
    }

    fn closed_recv_fail(&mut self) {
        // Welp, someone had asked us to lock and then died. Release the
        // lock and any resources acquired from the device driver.
        self.lock_holder = None;
        self.bitstream_loader = None;
    }

    fn lock(&mut self, msg: &userlib::RecvMessage) -> Result<(), RequestError> {
        if let Some(task) = self.lock_holder {
            // The fact that we received this message _at all_ means
            // that the sender matched our closed receive, but just
            // in case we have a server logic bug, let's check.
            assert!(task == msg.sender);
        }

        self.lock_holder = Some(msg.sender);
        ringbuf_entry!(Trace::Locked(msg.sender));
        Ok(())
    }

    fn release(
        &mut self,
        msg: &userlib::RecvMessage,
    ) -> Result<(), RequestError> {
        if let Some(task) = self.lock_holder {
            // The fact that we received this message _at all_ means
            // that the sender matched our closed receive, but just
            // in case we have a server logic bug, let's check.
            assert!(task == msg.sender);

            self.lock_holder = None;
            ringbuf_entry!(Trace::Released(msg.sender));
            Ok(())
        } else {
            Err(FpgaError::NotLocked.into())
        }
    }

    fn device_enabled(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError> {
        Ok(self.device.device_enabled()?)
    }

    fn set_device_enabled(
        &mut self,
        _: &RecvMessage,
        enabled: bool,
    ) -> Result<(), RequestError> {
        Ok(self.device.set_device_enabled(enabled)?)
    }

    fn reset_device(&mut self, _: &RecvMessage) -> Result<(), RequestError> {
        Ok(self.device.reset_device()?)
    }

    fn device_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<DeviceState, RequestError> {
        Ok(self.device.device_state()?)
    }

    fn device_id(&mut self, _: &RecvMessage) -> Result<u32, RequestError> {
        Ok(self.device.device_id()?)
    }

    fn user_design_enabled(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError> {
        Ok(self.device.user_design_enabled()?)
    }

    fn set_user_design_enabled(
        &mut self,
        _: &RecvMessage,
        enabled: bool,
    ) -> Result<(), RequestError> {
        Ok(self.device.set_user_design_enabled(enabled)?)
    }

    fn reset_user_design(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError> {
        Ok(self.device.reset_user_design()?)
    }

    fn start_bitstream_load(
        &mut self,
        _: &RecvMessage,
        bitstream_type: BitstreamType,
    ) -> Result<(), RequestError> {
        if self.bitstream_loader.is_some() {
            return Err(RequestError::Runtime(FpgaError::InvalidState));
        }

        self.bitstream_loader = Some(match bitstream_type {
            BitstreamType::Uncompressed => BitstreamLoader::Uncompressed(
                self.device.start_bitstream_load()?,
                0,
            ),
            BitstreamType::Compressed => BitstreamLoader::Compressed(
                gnarle::Decompressor::default(),
                self.device.start_bitstream_load()?,
                0,
            ),
        });

        ringbuf_entry!(Trace::StartBitstreamLoad(bitstream_type));
        Ok(())
    }

    fn continue_bitstream_load(
        &mut self,
        _: &RecvMessage,
        data: LenLimit<Leased<R, [u8]>, 128>,
    ) -> Result<(), RequestError> {
        data.read_range(0..data.len(), &mut self.buffer[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        let mut chunk = &self.buffer[..data.len()];
        let mut decompress_buffer = [0; 512];

        match &mut self.bitstream_loader {
            None => return Err(RequestError::Runtime(FpgaError::InvalidState)),
            Some(BitstreamLoader::Uncompressed(bitstream, len)) => {
                bitstream.continue_load(chunk)?;
                *len += chunk.len();
            }
            Some(BitstreamLoader::Compressed(decompressor, bitstream, len)) => {
                while !chunk.is_empty() {
                    let decompressed_chunk = gnarle::decompress(
                        decompressor,
                        &mut chunk,
                        &mut decompress_buffer,
                    );

                    // The compressor may have encountered a partial run at the
                    // end of the `chunk`, in which case `decompressed_chunk`
                    // will be empty since more data is needed before output is
                    // generated.
                    if decompressed_chunk.len() > 0 {
                        bitstream.continue_load(decompressed_chunk)?;
                        *len += decompressed_chunk.len();
                    }
                }
            }
        }

        ringbuf_entry!(Trace::ContinueBitstreamLoad(data.len()));
        Ok(())
    }

    fn finish_bitstream_load(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError> {
        match &mut self.bitstream_loader {
            None => return Err(RequestError::Runtime(FpgaError::InvalidState)),
            Some(BitstreamLoader::Uncompressed(bitstream, len)) => {
                ringbuf_entry!(Trace::FinishBitstreamLoad(*len));
                bitstream.finish_load()?;
            }
            Some(BitstreamLoader::Compressed(_, bitstream, len)) => {
                ringbuf_entry!(Trace::FinishBitstreamLoad(*len));
                bitstream.finish_load()?;
            }
        }

        self.bitstream_loader = None;
        Ok(())
    }

    fn user_design_read(
        &mut self,
        _: &userlib::RecvMessage,
        addr: u16,
        data: WriteDataLease,
    ) -> Result<(), RequestError> {
        let header = user_designRequestHeader {
            cmd: 0x1,
            addr: U16::new(addr),
        };

        let _lock = self.lock_user_design()?; // Released on function exit.

        self.device
            .user_design_write(header.as_bytes())
            .map_err(FpgaError::from)?;
        self.device
            .user_design_read(&mut self.buffer[..data.len()])
            .map_err(FpgaError::from)?;

        data.write_range(0..data.len(), &self.buffer[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        Ok(())
    }

    fn user_design_write(
        &mut self,
        _: &userlib::RecvMessage,
        op: WriteOp,
        addr: u16,
        data: ReadDataLease,
    ) -> Result<(), RequestError> {
        data.read_range(0..data.len(), &mut self.buffer[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        let header = user_designRequestHeader {
            cmd: u8::from(op),
            addr: U16::new(addr),
        };

        let _lock = self.lock_user_design()?; // Released on function exit.

        self.device
            .user_design_write(header.as_bytes())
            .map_err(FpgaError::from)?;
        self.device
            .user_design_write(&self.buffer[..data.len()])
            .map_err(FpgaError::from)?;

        Ok(())
    }
}

#[derive(AsBytes, Unaligned)]
#[repr(C)]
struct user_designRequestHeader {
    cmd: u8,
    addr: U16<byteorder::BigEndian>,
}

mod idl {
    use super::{BitstreamType, DeviceState, FpgaError, WriteOp};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
