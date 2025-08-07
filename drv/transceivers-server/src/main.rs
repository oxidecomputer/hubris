// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use counters::Count;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use static_cell::ClaimOnceCell;
use userlib::{set_timer_relative, task_slot, units::Celsius, RecvMessage};

use drv_fpga_api::FpgaError;
use drv_front_io_api::{
    transceivers::{
        FpgaI2CFailure, LogicalPort, LogicalPortMask, ModuleStatus, NUM_PORTS,
    },
    FrontIO, FrontIOError, FrontIOStatus, Reg,
};
use drv_transceivers_api::{
    TransceiversError, TRANSCEIVER_TEMPERATURE_SENSORS,
};
use task_sensor_api::{NoData, Sensor};
#[allow(unused_imports)]
use task_thermal_api::{Thermal, ThermalError, ThermalProperties};
use transceiver_messages::{mgmt::ManagementInterface, MAX_PACKET_SIZE};

use zerocopy::{FromBytes, FromZeros, IntoBytes};

mod udp; // UDP API is implemented in a separate file

task_slot!(FRONT_IO, front_io);
task_slot!(NET, net);
task_slot!(SENSOR, sensor);

#[cfg(feature = "thermal-control")]
task_slot!(THERMAL, thermal);

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq, Eq, Count)]
enum Trace {
    #[count(skip)]
    None,
    FrontIOStatus(#[count(children)] FrontIOStatus),
    LEDInit,
    LEDEnableError(FrontIOError),
    ModulePresenceUpdate(LogicalPortMask),
    TransceiversError(#[count(children)] TransceiversError),
    GotInterface(u8, ManagementInterface),
    UnknownInterface(u8, ManagementInterface),
    UnpluggedModule(usize),
    RemovedDisabledModuleThermalModel(usize),
    TemperatureReadError(usize, Reg::QSFP::PORT0_STATUS::ErrorEncoded),
    TemperatureReadUnexpectedError(usize, FpgaError),
    ThermalError(usize, ThermalError),
    GetInterfaceError(usize, Reg::QSFP::PORT0_STATUS::ErrorEncoded),
    GetInterfaceUnexpectedError(usize, FpgaError),
    InvalidPortStatusError(usize, u8),
    DisablingPorts(LogicalPortMask),
    DisableFailed(usize, LogicalPortMask),
    ClearDisabledPorts(LogicalPortMask),
}

counted_ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

/// After seeing this many NACKs or timeouts, we disable the port by policy.
///
/// This should be **very rare**: it requires a transceiver to correctly report
/// its type (SFF vs CMIS) over I2C when it's first plugged in, but then begin
/// NACKing or timing out while still physically present (according to the
/// `modprsl` pin).
///
/// Despite the weirdness of these pre-requisites, we've seen this happen once
/// already; without handling it, the thermal loop will eventually shut down the
/// whole system (because the transceiver stops reporting its temperature).
const MAX_CONSECUTIVE_ERRORS: u8 = 3;

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl {
    /// The FrontIO server is the interface to the transceivers and LED drivers
    front_io: FrontIO,
    net: task_net_api::Net,
    modules_present: LogicalPortMask,

    /// The Front IO board is not guaranteed to be present and ready
    front_io_status: FrontIOStatus,

    /// Modules that are physically present but disabled by Hubris
    disabled: LogicalPortMask,

    /// Number of consecutive NACKS seen on a given port
    consecutive_errors: [u8; NUM_PORTS as usize],

    /// Handle to write thermal models and presence to the `thermal` task
    #[cfg(feature = "thermal-control")]
    thermal_api: Thermal,

    /// Handle to write temperatures to the `sensors` task
    sensor_api: Sensor,

    /// Thermal models are populated by the host
    thermal_models: [Option<ThermalModel>; NUM_PORTS as usize],
}
#[derive(Copy, Clone)]
struct ThermalModel {
    /// What kind of transceiver is this?
    interface: ManagementInterface,

    /// What are its thermal properties, e.g. critical temperature?
    #[allow(dead_code)]
    model: ThermalProperties,
}

/// Controls how often we poll the transceivers (in milliseconds).
///
/// For transceivers that are present and include a thermal model, we measure
/// their temperature and send it to the `thermal` task.
const SPI_INTERVAL: u32 = 500;

impl ServerImpl {
    /// Returns the temperature from a CMIS transceiver.
    ///
    /// `port` is a logical port index, i.e. 0-31.
    fn read_cmis_temperature(
        &self,
        port: LogicalPort,
    ) -> Result<Celsius, FpgaError> {
        const CMIS_TEMPERATURE_MSB: u8 = 14; // CMIS, Table 8-9
        self.read_temperature_from_i16(port, CMIS_TEMPERATURE_MSB)
    }

    /// Returns the temperature from a SFF-8636 transceiver.
    ///
    /// `port` is a logical port index, i.e. 0-31.
    fn read_sff8636_temperature(
        &self,
        port: LogicalPort,
    ) -> Result<Celsius, FpgaError> {
        const SFF8636_TEMPERATURE_MSB: u8 = 22; // SFF-8636, Table 6-7
        self.read_temperature_from_i16(port, SFF8636_TEMPERATURE_MSB)
    }

    /// Trigger a read from the given port's given register, which is assumed to
    /// be an `i16` containing 1/256 °C.
    fn read_temperature_from_i16(
        &self,
        port: LogicalPort,
        reg: u8,
    ) -> Result<Celsius, FpgaError> {
        let result = match self.front_io.transceivers_setup_i2c_read(
            reg,
            2,
            port.as_mask(),
        ) {
            Ok(r) => r,
            Err(e) => return Err(FpgaError::ImplError(e as u8)),
        };

        if !result.error().is_empty() {
            return Err(FpgaError::CommsError);
        }

        #[derive(Copy, Clone, FromBytes, IntoBytes)]
        #[repr(C)]
        struct Temperature {
            temperature: zerocopy::I16<zerocopy::BigEndian>,
        }

        let mut out = Temperature::new_zeroed();
        let status = self
            .front_io
            .transceivers_get_i2c_status_and_read_buffer(
                port,
                out.as_mut_bytes(),
            )
            .map_err(|e| FpgaError::ImplError(e as u8))?;

        if status.error == FpgaI2CFailure::NoError {
            // "Internally measured free side device temperatures are
            // represented as a 16-bit signed twos complement value in
            // increments of 1/256 degrees Celsius"
            //
            // - SFF-8636 rev 2.10a, Section 6.2.4
            Ok(Celsius(out.temperature.get() as f32 / 256.0))
        } else {
            Err(FpgaError::ImplError(status.error as u8))
        }
    }

    fn get_transceiver_interface(
        &mut self,
        port: LogicalPort,
    ) -> Result<ManagementInterface, FpgaError> {
        let result = match self.front_io.transceivers_setup_i2c_read(
            0,
            1,
            port.as_mask(),
        ) {
            Ok(r) => r,
            Err(e) => return Err(FpgaError::ImplError(e as u8)),
        };

        if !result.error().is_empty() {
            return Err(FpgaError::CommsError);
        }

        let mut out = [0u8; 1]; // [SFF8024Identifier]

        // Wait for the I2C transaction to complete
        let status = self
            .front_io
            .transceivers_get_i2c_status_and_read_buffer(port, &mut out)
            .map_err(|e| FpgaError::ImplError(e as u8))?;

        if status.error == FpgaI2CFailure::NoError {
            match out[0] {
                0x1E => Ok(ManagementInterface::Cmis),
                0x0D | 0x11 => Ok(ManagementInterface::Sff8636),
                i => Ok(ManagementInterface::Unknown(i)),
            }
        } else {
            // TODO: how should we handle this?
            // Right now, we'll retry on the next pass through the loop.
            Err(FpgaError::ImplError(status.error as u8))
        }
    }

    /// Converts from a `ManagementInterface` to a `ThermalModel`
    ///
    /// If the management interface is unknown, returns `None` instead
    ///
    /// Logs debug information to our ringbuf, tagged with the logical port.
    fn decode_interface(
        &mut self,
        p: LogicalPort,
        interface: ManagementInterface,
    ) -> Option<ThermalModel> {
        match interface {
            ManagementInterface::Sff8636 | ManagementInterface::Cmis => {
                ringbuf_entry!(Trace::GotInterface(p.0, interface));
                // TODO: this is made up
                Some(ThermalModel {
                    interface,
                    model: ThermalProperties {
                        target_temperature: Celsius(65.0),
                        critical_temperature: Celsius(70.0),
                        power_down_temperature: Celsius(80.0),
                        temperature_slew_deg_per_sec: 0.5,
                    },
                })
            }
            ManagementInterface::Unknown(..) => {
                // We won't load Unknown transceivers into the thermal loop;
                // otherwise, the fans would spin up.
                ringbuf_entry!(Trace::UnknownInterface(p.0, interface));
                None
            }
        }
    }

    fn update_thermal_loop(&mut self, status: ModuleStatus) {
        #[allow(clippy::needless_range_loop)]
        for i in 0..self.thermal_models.len() {
            let port = LogicalPort(i as u8);
            let mask = 1 << i;
            let operational = (!status.modprsl & mask) != 0
                && (status.power_good & mask) != 0
                && (status.resetl & mask) != 0
                && (self.disabled & port).is_empty();

            // A wild transceiver just appeared!  Read it to decide whether it's
            // using SFF-8636 or CMIS.
            if operational && self.thermal_models[i].is_none() {
                match self.get_transceiver_interface(port) {
                    Ok(interface) => {
                        self.thermal_models[i] =
                            self.decode_interface(port, interface)
                    }
                    Err(FpgaError::ImplError(e)) => {
                        match Reg::QSFP::PORT0_STATUS::ErrorEncoded::try_from(e)
                        {
                            Ok(val) => {
                                ringbuf_entry!(Trace::GetInterfaceError(i, val))
                            }
                            Err(_) => {
                                // Error code cannot be decoded
                                ringbuf_entry!(Trace::InvalidPortStatusError(
                                    i, e
                                ))
                            }
                        }
                    }
                    Err(e) => {
                        // Not much we can do here if reading failed
                        ringbuf_entry!(Trace::GetInterfaceUnexpectedError(
                            i, e
                        ));
                    }
                }
            } else if !operational && self.thermal_models[i].is_some() {
                #[cfg(feature = "thermal-control")]
                {
                    // This transceiver went away; remove it from the thermal loop
                    if let Err(e) = self.thermal_api.remove_dynamic_input(i) {
                        ringbuf_entry!(Trace::ThermalError(i, e));
                    }
                }

                // Tell the `sensor` task that this device is no longer present
                self.sensor_api.nodata_now(
                    TRANSCEIVER_TEMPERATURE_SENSORS[i],
                    NoData::DeviceNotPresent,
                );

                if (self.disabled & port).is_empty() {
                    ringbuf_entry!(Trace::UnpluggedModule(i));
                } else {
                    ringbuf_entry!(Trace::RemovedDisabledModuleThermalModel(i));
                }
                self.thermal_models[i] = None;
            }
        }

        // Accumulate ports to disable (but don't disable them in the loop), to
        // avoid issues with the borrow checker.
        let mut to_disable = LogicalPortMask(0);
        for (i, m) in self.thermal_models.iter().enumerate() {
            let port = LogicalPort(i as u8);
            let m = match m {
                Some(m) => m,
                None => continue,
            };

            #[cfg(feature = "thermal-control")]
            {
                // *Always* post the thermal model over to the thermal task, so
                // that the thermal task still has it in case of restart.  This
                // will return a `NotInAutoMode` error if the thermal loop is in
                // manual mode; this is harmless and will be ignored (instead of
                // cluttering up the logs).
                match self.thermal_api.update_dynamic_input(i, m.model) {
                    Ok(()) | Err(ThermalError::NotInAutoMode) => (),
                    Err(e) => ringbuf_entry!(Trace::ThermalError(i, e)),
                }
            }

            let temperature = match m.interface {
                ManagementInterface::Cmis => self.read_cmis_temperature(port),
                ManagementInterface::Sff8636 => {
                    self.read_sff8636_temperature(port)
                }
                ManagementInterface::Unknown(..) => {
                    // We should never get here, because we only assign
                    // `self.thermal_models[i]` if the management interface is
                    // known.
                    continue;
                }
            };
            let mut got_error = false;
            match temperature {
                Ok(t) => {
                    // We got a temperature! Send it over to the thermal task
                    self.sensor_api
                        .post_now(TRANSCEIVER_TEMPERATURE_SENSORS[i], t.0);
                }
                // We failed to read a temperature :(
                //
                // This could be because someone unplugged the transceiver
                // at exactly the right time, in which case, the error will
                // be transient (and we'll remove the transceiver on the
                // next pass through this function).
                Err(FpgaError::ImplError(e)) => {
                    use Reg::QSFP::PORT0_STATUS::ErrorEncoded;
                    match ErrorEncoded::try_from(e) {
                        Ok(val) => {
                            got_error |= matches!(
                                val,
                                ErrorEncoded::I2CAddressNack
                                    | ErrorEncoded::I2CSclStretchTimeout
                            );
                            ringbuf_entry!(Trace::TemperatureReadError(i, val))
                        }
                        Err(_) => {
                            // Error code cannot be decoded
                            ringbuf_entry!(Trace::InvalidPortStatusError(i, e))
                        }
                    }
                }
                Err(e) => {
                    ringbuf_entry!(Trace::TemperatureReadUnexpectedError(i, e));
                }
            }

            self.consecutive_errors[i] = if got_error {
                self.consecutive_errors[i].saturating_add(1)
            } else {
                0
            };

            if self.consecutive_errors[i] >= MAX_CONSECUTIVE_ERRORS {
                to_disable.set(port);
            }
        }
        if !to_disable.is_empty() {
            self.disable_ports(to_disable);
        }
    }

    fn disable_ports(&mut self, mask: LogicalPortMask) {
        ringbuf_entry!(Trace::DisablingPorts(mask));
        for (step, f) in [
            FrontIO::transceivers_assert_reset,
            FrontIO::transceivers_deassert_lpmode,
            FrontIO::transceivers_disable_power,
        ]
        .iter()
        .enumerate()
        {
            let err = f(&mut self.front_io, mask).error();
            if !err.is_empty() {
                ringbuf_entry!(Trace::DisableFailed(step, err));
            }
        }
        self.disabled |= mask;
        // We don't modify self.thermal_models here; that's left to
        // `update_thermal_loop`, which is in charge of communicating with
        // the `sensors` and `thermal` tasks.
    }

    fn handle_spi_loop(&mut self) {
        // Query module presence as this drives other state
        let xcvr_status = self.front_io.transceivers_status();

        let modules_present = LogicalPortMask(!xcvr_status.status.modprsl);
        if modules_present != self.modules_present {
            // check to see if any disabled ports had their modules removed and
            // allow their power to be turned on when a module is reinserted
            let disabled_ports_removed =
                self.modules_present & !modules_present & self.disabled;
            if !disabled_ports_removed.is_empty() {
                self.disabled &= !disabled_ports_removed;
                self.front_io
                    .transceivers_enable_power(disabled_ports_removed);
                ringbuf_entry!(Trace::ClearDisabledPorts(
                    disabled_ports_removed
                ));
            }

            self.modules_present = modules_present;
            ringbuf_entry!(Trace::ModulePresenceUpdate(modules_present));
        }

        self.update_thermal_loop(xcvr_status.status);
    }
}

////////////////////////////////////////////////////////////////////////////////

impl idl::InOrderTransceiversImpl for ServerImpl {
    fn ping(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::SOCKET_MASK | notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        if self.front_io_status == FrontIOStatus::Ready {
            self.handle_spi_loop();
        }

        set_timer_relative(SPI_INTERVAL, notifications::TIMER_MASK);
    }
}

#[export_name = "main"]
fn main() -> ! {
    // This is a temporary workaround that makes sure the FPGAs are up
    // before we start doing things with them. A more sophisticated
    // notification system will be put in place.
    let front_io = FrontIO::from(FRONT_IO.get_task_id());

    let net = task_net_api::Net::from(NET.get_task_id());
    let sensor_api = Sensor::from(SENSOR.get_task_id());

    let (tx_data_buf, rx_data_buf) = {
        static BUFS: ClaimOnceCell<(
            [u8; MAX_PACKET_SIZE],
            [u8; MAX_PACKET_SIZE],
        )> = ClaimOnceCell::new(([0; MAX_PACKET_SIZE], [0; MAX_PACKET_SIZE]));
        BUFS.claim()
    };

    #[cfg(feature = "thermal-control")]
    let thermal_api = Thermal::from(THERMAL.get_task_id());

    let mut server = ServerImpl {
        front_io,
        net,
        modules_present: LogicalPortMask(0),
        front_io_status: FrontIOStatus::NotPresent,
        disabled: LogicalPortMask(0),
        consecutive_errors: [0; NUM_PORTS as usize],
        #[cfg(feature = "thermal-control")]
        thermal_api,
        sensor_api,
        thermal_models: [None; NUM_PORTS as usize],
    };

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    set_timer_relative(0, notifications::TIMER_MASK);
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        // We do this check within the main loop because we still want to
        // service any requests from `net` even if a front IO board is not ready
        // If a board is ready, attempt to initialize its LED drivers
        if server.front_io_status == FrontIOStatus::Ready {
            if let Err(e) = server.front_io.leds_enable() {
                ringbuf_entry!(Trace::LEDEnableError(e));
            };
        } else {
            userlib::hl::sleep_for(5);
            server.front_io_status = server.front_io.board_status();
            ringbuf_entry!(Trace::FrontIOStatus(server.front_io_status));
        }

        // monitor messages from the host
        server
            .check_net(tx_data_buf.as_mut_slice(), rx_data_buf.as_mut_slice());

        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
