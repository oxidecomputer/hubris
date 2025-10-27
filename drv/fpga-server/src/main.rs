// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing FPGA devices.

#![no_std]
#![no_main]

use ringbuf::*;
use userlib::{task_slot, RecvMessage, TaskId};
use zerocopy::{byteorder, Immutable, IntoBytes, KnownLayout, Unaligned, U16};

use drv_fpga_api::{BitstreamType, DeviceState, FpgaError, ReadOp, WriteOp};
use drv_fpga_devices::{ecp5, Fpga, FpgaBitstream, FpgaUserDesign};
use drv_spi_api::SpiServer;
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use idol_runtime::{ClientError, Leased, LenLimit, R, W};

task_slot!(SYS, sys);

cfg_if::cfg_if! {
    if #[cfg(feature = "front_io")] {
        task_slot!(I2C, i2c_driver);
        include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
    }
}

cfg_if::cfg_if! {
    // Select local vs server SPI communication
    if #[cfg(feature = "use-spi-core")] {
        /// Claims the SPI core.
        ///
        /// This function can only be called once, and will panic otherwise!
        pub fn claim_spi(sys: &sys_api::Sys)
            -> drv_stm32h7_spi_server_core::SpiServerCore
        {
            drv_stm32h7_spi_server_core::declare_spi_core!(
                sys.clone(), notifications::SPI_IRQ_MASK)
        }
    } else {
        pub fn claim_spi(_sys: &sys_api::Sys) -> drv_spi_api::Spi {
            task_slot!(SPI, spi_driver);
            drv_spi_api::Spi::from(SPI.get_task_id())
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Trace {
    None,
    DeviceId(u8, u32),
    StartBitstreamLoad(u8, BitstreamType),
    ContinueBitstreamLoad(usize),
    FinishBitstreamLoad(usize),
    Locked(TaskId),
    Released(TaskId),
}
ringbuf!(Trace, 64, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let sys = Sys::from(SYS.get_task_id());
    let spi = claim_spi(&sys);

    cfg_if::cfg_if! {
        if #[cfg(all(feature = "mainboard", feature = "front_io"))] {
            compile_error!("Cannot enable both mainboard and front_io simultaneously");
        } else if #[cfg(all(any(target_board = "sidecar-b",
                                target_board = "sidecar-c",
                                target_board = "sidecar-d"),
                            feature = "mainboard"))] {
            let configuration_port =
                spi.device(drv_spi_api::devices::ECP5_MAINBOARD_FPGA);
            let user_design =
                spi.device(drv_spi_api::devices::ECP5_MAINBOARD_USER_DESIGN);

            let driver = drv_fpga_devices::ecp5_spi::Ecp5UsingSpi {
                sys,
                done: sys_api::Port::J.pin(15),
                init_n: sys_api::Port::J.pin(12),
                program_n: sys_api::Port::J.pin(13),
                configuration_port,
                user_design,
                user_design_reset_n: sys_api::Port::J.pin(14),
                user_design_reset_duration: ecp5::USER_DESIGN_RESET_DURATION,
            };
            driver.configure_gpio();

            let devices = [ecp5::Ecp5::new(driver)];
        } else if #[cfg(all(any(target_board = "sidecar-b",
                                target_board = "sidecar-c",
                                target_board = "sidecar-d",
                                target_board = "medusa-a"),
                            feature = "front_io"))] {
            let configuration_port =
                spi.device(drv_spi_api::devices::ECP5_FRONT_IO_FPGA);
            let user_design =
                spi.device(drv_spi_api::devices::ECP5_FRONT_IO_USER_DESIGN);

            use drv_i2c_devices::pca9538::*;
            use drv_fpga_devices::ecp5_spi_mux_pca9538::*;

            let gpio = Pca9538::new(
                i2c_config::devices::pca9538(I2C.get_task_id())[0],
            );
            let driver = Driver::new(DriverConfig {
                sys,
                gpio,
                spi_mux_select: sys_api::Port::F.pin(3),
                configuration_port,
                user_design,
                user_design_reset_duration: ecp5::USER_DESIGN_RESET_DURATION,
            });

            driver.init().unwrap();

            // Loop forever until devices come up
            let devices = loop {
                let device0_pins = DevicePins{
                    done: PinSet::pin(3),
                    init_n: PinSet::pin(0),
                    program_n: PinSet::pin(1),
                    user_design_reset_n: PinSet::pin(2),
                };
                let device1_pins = DevicePins {
                    done: PinSet::pin(7),
                    init_n: PinSet::pin(4),
                    program_n: PinSet::pin(5),
                    user_design_reset_n: PinSet::pin(6),
                };
                match driver.init_devices(device0_pins, device1_pins) {
                    Ok(devices) => break devices,
                    Err(_) => userlib::hl::sleep_for(10),
                }
            };
        } else if #[cfg(target_board = "gimletlet-2")] {
            // Hard-coding because the TOML file doesn't specify great names
            let configuration_port = spi.device(0);
            let user_design = spi.device(1);
            let driver = drv_fpga_devices::ecp5_spi::Ecp5UsingSpi {
                sys,
                done: sys_api::Port::E.pin(15),
                init_n: sys_api::Port::D.pin(12),
                program_n: sys_api::Port::B.pin(10),
                configuration_port,
                user_design,
                user_design_reset_n: sys_api::Port::D.pin(11),
                user_design_reset_duration: ecp5::USER_DESIGN_RESET_DURATION,
            };
            driver.configure_gpio();

            let devices = [ecp5::Ecp5::new(driver)];
        } else if #[cfg(any(target_board = "minibar-a", target_board = "minibar-b"))] {
            let configuration_port =
                spi.device(drv_spi_api::devices::ECP5_FPGA);
            let user_design =
                spi.device(drv_spi_api::devices::ECP5_USER_DESIGN);

            let driver = drv_fpga_devices::ecp5_spi::Ecp5UsingSpi {
                sys,
                done: sys_api::Port::J.pin(14),
                init_n: sys_api::Port::J.pin(12),
                program_n: sys_api::Port::J.pin(13),
                configuration_port,
                user_design,
                user_design_reset_n: sys_api::Port::J.pin(15),
                user_design_reset_duration: ecp5::USER_DESIGN_RESET_DURATION,
            };
            driver.configure_gpio();

            let devices = [ecp5::Ecp5::new(driver)];
        } else {
            compile_error!("Board is not supported by drv/fpga-server");
        }
    }

    let mut incoming = [0u8; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        lock_holder: None,
        devices: &devices,
        buffer: [0u8; 128],
        bitstream_loader: None,
    };

    for (i, device) in server.devices.iter().enumerate() {
        if let Ok(DeviceState::AwaitingBitstream) = device.device_state() {
            ringbuf_entry!(Trace::DeviceId(
                i as u8,
                device.device_id().unwrap()
            ));
        }
    }

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

enum BitstreamLoader<'a, Device: Fpga<'a>> {
    Uncompressed(Device::Bitstream, usize),
    Compressed(gnarle::Decompressor, Device::Bitstream, usize),
}

struct LockState {
    task: userlib::TaskId,
    device_index: usize,
}

struct ServerImpl<'a, Device: Fpga<'a> + FpgaUserDesign> {
    lock_holder: Option<LockState>,
    devices: &'a [Device],
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
    fn check_lock_and_get_device(
        &self,
        caller: userlib::TaskId,
        device_index: u8,
    ) -> Result<&'a Device, FpgaError> {
        let device_index = usize::from(device_index);

        if let Some(lock_state) = &self.lock_holder {
            // The fact that we received this message _at all_ means
            // that the sender matched our closed receive, but just
            // in case we have a server logic bug, let's check.
            assert!(lock_state.task == caller);

            if lock_state.device_index != device_index {
                return Err(FpgaError::BadDevice);
            }
        }

        self.devices.get(device_index).ok_or(FpgaError::BadDevice)
    }

    fn lock_user_design(
        &self,
        caller: userlib::TaskId,
        device_index: u8,
    ) -> Result<UserDesignLock<'a, Device>, FpgaError> {
        let device = self.check_lock_and_get_device(caller, device_index)?;

        device.user_design_lock()?;
        Ok(UserDesignLock(device))
    }
}

type RequestError = idol_runtime::RequestError<FpgaError>;
type ReadDataLease = LenLimit<Leased<R, [u8]>, 128>;

impl<'a, Device: Fpga<'a> + FpgaUserDesign> idl::InOrderFpgaImpl
    for ServerImpl<'a, Device>
{
    fn recv_source(&self) -> Option<userlib::TaskId> {
        self.lock_holder.as_ref().map(|s| s.task)
    }

    fn closed_recv_fail(&mut self) {
        // Welp, someone had asked us to lock and then died. Release the
        // lock and any resources acquired from the device driver.
        self.lock_holder = None;
        self.bitstream_loader = None;
    }

    fn lock(
        &mut self,
        msg: &userlib::RecvMessage,
        device_index: u8,
    ) -> Result<(), RequestError> {
        match &self.lock_holder {
            Some(lock_state) => {
                assert!(lock_state.task == msg.sender);
                Err(RequestError::Runtime(FpgaError::AlreadyLocked))
            }
            None => {
                ringbuf_entry!(Trace::Locked(msg.sender));

                self.lock_holder = Some(LockState {
                    task: msg.sender,
                    device_index: usize::from(device_index),
                });
                Ok(())
            }
        }
    }

    fn release(
        &mut self,
        msg: &userlib::RecvMessage,
    ) -> Result<(), RequestError> {
        if let Some(lock_state) = &self.lock_holder {
            assert!(lock_state.task == msg.sender);
            ringbuf_entry!(Trace::Released(msg.sender));

            self.lock_holder = None;
            Ok(())
        } else {
            Err(FpgaError::NotLocked.into())
        }
    }

    fn device_enabled(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
    ) -> Result<bool, RequestError> {
        self.check_lock_and_get_device(msg.sender, device_index)?
            .device_enabled()
            .map_err(Into::into)
    }

    fn set_device_enabled(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
        enabled: bool,
    ) -> Result<(), RequestError> {
        self.check_lock_and_get_device(msg.sender, device_index)?
            .set_device_enabled(enabled)
            .map_err(Into::into)
    }

    fn reset_device(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
    ) -> Result<(), RequestError> {
        self.check_lock_and_get_device(msg.sender, device_index)?
            .reset_device()
            .map_err(Into::into)
    }

    fn device_state(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
    ) -> Result<DeviceState, RequestError> {
        self.check_lock_and_get_device(msg.sender, device_index)?
            .device_state()
            .map_err(Into::into)
    }

    fn device_id(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
    ) -> Result<u32, RequestError> {
        self.check_lock_and_get_device(msg.sender, device_index)?
            .device_id()
            .map_err(Into::into)
    }

    fn user_design_enabled(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
    ) -> Result<bool, RequestError> {
        self.check_lock_and_get_device(msg.sender, device_index)?
            .user_design_enabled()
            .map_err(Into::into)
    }

    fn set_user_design_enabled(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
        enabled: bool,
    ) -> Result<(), RequestError> {
        self.check_lock_and_get_device(msg.sender, device_index)?
            .set_user_design_enabled(enabled)
            .map_err(Into::into)
    }

    fn reset_user_design(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
    ) -> Result<(), RequestError> {
        self.check_lock_and_get_device(msg.sender, device_index)?
            .reset_user_design()
            .map_err(Into::into)
    }

    fn start_bitstream_load(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
        bitstream_type: BitstreamType,
    ) -> Result<(), RequestError> {
        if self.bitstream_loader.is_some() {
            return Err(RequestError::Runtime(FpgaError::InvalidState));
        }

        let device =
            self.check_lock_and_get_device(msg.sender, device_index)?;

        self.bitstream_loader = Some(match bitstream_type {
            BitstreamType::Uncompressed => {
                BitstreamLoader::Uncompressed(device.start_bitstream_load()?, 0)
            }
            BitstreamType::Compressed => BitstreamLoader::Compressed(
                gnarle::Decompressor::default(),
                device.start_bitstream_load()?,
                0,
            ),
        });

        ringbuf_entry!(Trace::StartBitstreamLoad(device_index, bitstream_type));
        Ok(())
    }

    fn continue_bitstream_load(
        &mut self,
        _: &RecvMessage,
        data: ReadDataLease,
    ) -> Result<(), RequestError> {
        data.read_range(0..data.len(), &mut self.buffer[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        let mut chunk = &self.buffer[..data.len()];

        match &mut self.bitstream_loader {
            None => return Err(RequestError::Runtime(FpgaError::InvalidState)),
            Some(BitstreamLoader::Uncompressed(bitstream, len)) => {
                bitstream.continue_load(chunk)?;
                *len += chunk.len();
            }
            Some(BitstreamLoader::Compressed(decompressor, bitstream, len)) => {
                let mut decompress_buffer = [0; 512];

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
                    if !decompressed_chunk.is_empty() {
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
        msg: &RecvMessage,
        device_index: u8,
        op: ReadOp,
        addr: u16,
        data: Leased<W, [u8]>,
    ) -> Result<(), RequestError> {
        let header = UserDesignRequestHeader {
            cmd: u8::from(op),
            addr: U16::new(addr),
        };

        // Released on function exit.
        let lock = self.lock_user_design(msg.sender, device_index)?;

        lock.0.user_design_write(header.as_bytes())?;

        let mut index = 0;
        while index < data.len() {
            let chunk_size = (data.len() - index).min(self.buffer.len());
            lock.0.user_design_read(&mut self.buffer[..chunk_size])?;

            data.write_range(
                index..(index + chunk_size),
                &self.buffer[..chunk_size],
            )
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            index += chunk_size;
        }

        Ok(())
    }

    fn user_design_write(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
        op: WriteOp,
        addr: u16,
        data: Leased<R, [u8]>,
    ) -> Result<(), RequestError> {
        let header = UserDesignRequestHeader {
            cmd: u8::from(op),
            addr: U16::new(addr),
        };

        // Released on function exit.
        let lock = self.lock_user_design(msg.sender, device_index)?;

        lock.0.user_design_write(header.as_bytes())?;

        let mut index = 0;
        while index < data.len() {
            let chunk_size = (data.len() - index).min(self.buffer.len());
            data.read_range(
                index..(index + chunk_size),
                &mut self.buffer[..chunk_size],
            )
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            lock.0.user_design_write(&self.buffer[..chunk_size])?;
            index += chunk_size;
        }

        Ok(())
    }

    fn user_design_read_reg(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
        addr: u16,
    ) -> Result<u8, RequestError> {
        let header = UserDesignRequestHeader {
            cmd: 0x1,
            addr: U16::new(addr),
        };

        // Released on function exit.
        let lock = self.lock_user_design(msg.sender, device_index)?;

        lock.0.user_design_write(header.as_bytes())?;
        lock.0.user_design_read(&mut self.buffer[..1])?;

        Ok(self.buffer[0])
    }

    fn user_design_write_reg(
        &mut self,
        msg: &RecvMessage,
        device_index: u8,
        op: WriteOp,
        addr: u16,
        value: u8,
    ) -> Result<(), RequestError> {
        let header = UserDesignRequestHeader {
            cmd: u8::from(op),
            addr: U16::new(addr),
        };

        // Released on function exit.
        let lock = self.lock_user_design(msg.sender, device_index)?;

        lock.0.user_design_write(header.as_bytes())?;
        lock.0.user_design_write(value.as_bytes())?;

        Ok(())
    }
}

impl<'a, Device: Fpga<'a> + FpgaUserDesign> idol_runtime::NotificationHandler
    for ServerImpl<'a, Device>
{
    fn current_notification_mask(&self) -> u32 {
        // We do not expect notifications.
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

#[derive(IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct UserDesignRequestHeader {
    cmd: u8,
    addr: U16<byteorder::BigEndian>,
}

mod idl {
    use super::{BitstreamType, DeviceState, FpgaError, ReadOp, WriteOp};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
