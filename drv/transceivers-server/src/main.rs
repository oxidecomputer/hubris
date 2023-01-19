// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_fpga_api::FpgaError;
use drv_i2c_devices::pca9956b::Error;
use drv_sidecar_front_io::{
    leds::FullErrorSummary,
    leds::Leds,
    transceivers::{LogicalPort, LogicalPortMask, Transceivers},
    Reg,
};
use drv_sidecar_seq_api::{SeqError, Sequencer};
use drv_transceivers_api::{
    ModulesStatus, PowerState, PowerStatesAll, TransceiversError, NUM_PORTS,
    PAGE_SIZE_BYTES, TRANSCEIVER_TEMPERATURE_SENSORS,
};
use hubpack::SerializedSize;
use idol_runtime::{
    ClientError, Leased, NotificationHandler, RequestError, R, W,
};
use ringbuf::*;
use task_sensor_api::{NoData, Sensor, SensorError};
use task_thermal_api::{Thermal, ThermalError, ThermalProperties};
use transceiver_messages::mgmt::ManagementInterface;
use userlib::{units::Celsius, *};
use zerocopy::{AsBytes, FromBytes};

mod udp; // UDP API is implemented in a separate file

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(SEQ, seq);
task_slot!(NET, net);
task_slot!(THERMAL, thermal);
task_slot!(SENSOR, sensor);

// Both incoming and outgoing messages use the Message type, so we use it to
// size our Tx / Rx buffers.
const MAX_UDP_MESSAGE_SIZE: usize =
    transceiver_messages::message::Message::MAX_SIZE
        + transceiver_messages::MAX_PAYLOAD_SIZE;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq, Eq)]
enum Trace {
    None,
    FrontIOReady(bool),
    FrontIOSeqErr(SeqError),
    LEDInit,
    LEDInitComplete,
    LEDInitError(Error),
    LEDErrorSummary(FullErrorSummary),
    LEDUninitialized,
    LEDUpdateError(Error),
    ModulePresenceUpdate(u32),
    TransceiversError(TransceiversError),
    GotInterface(usize, ManagementInterface),
    UnpluggedModule(usize),
    TemperatureReadError(usize, FpgaError),
    SensorError(usize, SensorError),
    ThermalError(usize, ThermalError),
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl {
    transceivers: Transceivers,
    leds: Leds,
    net: task_net_api::Net,
    modules_present: u32,
    led_error: FullErrorSummary,
    leds_initialized: bool,

    /// Handle to write thermal models and presence to the `thermal` task
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
    model: ThermalProperties,
}

/// Controls how often we poll the transceivers (in milliseconds).
///
/// Polling the transceivers serves a few functions:
/// - Transceiver presence is used to control LEDs on the front IO board
/// - For transceivers that are present and include a thermal model, we measure
///   their temperature and send it to the `thermal` task.
const TIMER_INTERVAL: u64 = 500;

// Errors are being suppressed here due to a miswiring of the I2C bus at the
// LED controller parts. They will not be accessible without rework to older
// hardware, and newer (correct) hardware will be replacing the old stuff
// very soon.
//
// TODO: remove conditional compilation path once sidecar-a is sunset
#[cfg(target_board = "sidecar-a")]
impl ServerImpl {
    fn led_init(&mut self) {
        let _ = self.leds.initialize_current();
        let _ = self.leds.turn_on_system_led();
        self.leds_initialized = true;
        ringbuf_entry!(Trace::LEDInitComplete);
    }

    fn led_update(&self, presence: u32) {
        let _ = self.leds.update_led_state(presence);
    }
}

#[cfg(not(target_board = "sidecar-a"))]
impl ServerImpl {
    fn led_init(&mut self) {
        match self
            .leds
            .initialize_current()
            .and(self.leds.turn_on_system_led())
        {
            Ok(_) => {
                self.leds_initialized = true;
                ringbuf_entry!(Trace::LEDInitComplete);
            }
            Err(e) => ringbuf_entry!(Trace::LEDInitError(e)),
        };
    }

    fn led_update(&self, presence: u32) {
        if self.leds_initialized {
            match self.leds.update_led_state(presence) {
                Ok(_) => (),
                Err(e) => ringbuf_entry!(Trace::LEDUpdateError(e)),
            }
        }
    }
}

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
        self.transceivers.setup_i2c_read(reg, 2, port.as_mask())?;

        #[derive(Copy, Clone, FromBytes, AsBytes)]
        #[repr(C)]
        struct StatusAndTemperature {
            status: u8,
            temperature: zerocopy::I16<zerocopy::BigEndian>,
        }

        loop {
            let mut out = StatusAndTemperature::new_zeroed();
            self.transceivers
                .get_i2c_status_and_read_buffer(port, out.as_bytes_mut())?;
            if out.status & Reg::QSFP::PORT0_STATUS::BUSY == 0 {
                if out.status & Reg::QSFP::PORT0_STATUS::ERROR != 0 {
                    return Err(FpgaError::ImplError(0));
                } else {
                    // "Internally measured free side device temperatures are
                    // represented as a 16-bit signed twos complement value in
                    // increments of 1/256 degrees Celsius"
                    //
                    // - SFF-8636 rev 2.10a, Section 6.2.4
                    return Ok(Celsius(out.temperature.get() as f32 / 256.0));
                }
            }
            userlib::hl::sleep_for(1);
        }
    }

    fn get_transceiver_interface(
        &mut self,
        port: LogicalPort,
    ) -> Result<ManagementInterface, FpgaError> {
        self.transceivers.setup_i2c_read(0, 1, port.as_mask())?;
        let mut out = [0u8; 2]; // [status, SFF8024Identifier]

        // Wait for the I2C transaction to complete
        loop {
            self.transceivers
                .get_i2c_status_and_read_buffer(port, &mut out)?;
            if out[0] & Reg::QSFP::PORT0_STATUS::BUSY == 0 {
                break;
            }
            userlib::hl::sleep_for(1);
        }

        if out[0] & Reg::QSFP::PORT0_STATUS::ERROR == 0 {
            match out[1] {
                0x1E => Ok(ManagementInterface::Cmis),
                0x0D | 0x11 => Ok(ManagementInterface::Sff8636),
                i => Ok(ManagementInterface::Unknown(i)),
            }
        } else {
            // TODO: how should we handle this?
            // Right now, we'll retry on the next pass through the loop.
            Err(FpgaError::ImplError(0))
        }
    }

    fn update_thermal_loop(&mut self, status: ModulesStatus) {
        for i in 0..self.thermal_models.len() {
            let port = LogicalPort(i as u8);
            let mask = 1 << i;
            let powered =
                (status.present & mask) != 0 && (status.power_good & mask) != 0;

            // A wild transceiver just appeared!  Read it to decide whether it's
            // using SFF-8636 or CMIS.
            if powered && self.thermal_models[i].is_none() {
                match self.get_transceiver_interface(port) {
                    Ok(interface) => {
                        ringbuf_entry!(Trace::GotInterface(i, interface));
                        // TODO: this is made up
                        self.thermal_models[i] = Some(ThermalModel {
                            interface,
                            model: ThermalProperties {
                                target_temperature: Celsius(65.0),
                                critical_temperature: Celsius(70.0),
                                power_down_temperature: Celsius(80.0),
                                temperature_slew_deg_per_sec: 0.5,
                            },
                        });
                    }
                    Err(e) => {
                        // Not much we can do here if reading failed
                        ringbuf_entry!(Trace::TemperatureReadError(i, e));
                    }
                }
            } else if !powered && self.thermal_models[i].is_some() {
                // This transceiver went away; remove it from the thermal loop
                if let Err(e) = self.thermal_api.remove_dynamic_input(i) {
                    ringbuf_entry!(Trace::ThermalError(i, e));
                }

                // Tell the `sensor` task that this device is no longer present
                if let Err(e) = self.sensor_api.nodata_now(
                    TRANSCEIVER_TEMPERATURE_SENSORS[i],
                    NoData::DeviceNotPresent,
                ) {
                    ringbuf_entry!(Trace::SensorError(i, e));
                }

                ringbuf_entry!(Trace::UnpluggedModule(i));
                self.thermal_models[i] = None;
            }
        }

        for (i, m) in self.thermal_models.iter().enumerate() {
            let port = LogicalPort(i as u8);
            let m = match m {
                Some(m) => m,
                None => continue,
            };

            // *Always* post the thermal model over to the thermal task, so that
            // the thermal task still has it in case of restart.
            if let Err(e) = self.thermal_api.update_dynamic_input(i, m.model) {
                ringbuf_entry!(Trace::ThermalError(i, e));
            }

            let temperature = match m.interface {
                ManagementInterface::Cmis => self.read_cmis_temperature(port),
                ManagementInterface::Sff8636 => {
                    self.read_sff8636_temperature(port)
                }
                ManagementInterface::Unknown(..) => {
                    // TODO: what should we do here?
                    continue;
                }
            };
            match temperature {
                Ok(t) => {
                    // We got a temperature! Send it over to the thermal task
                    if let Err(e) = self
                        .sensor_api
                        .post_now(TRANSCEIVER_TEMPERATURE_SENSORS[i], t.0)
                    {
                        ringbuf_entry!(Trace::SensorError(i, e));
                    }
                }
                Err(e) => {
                    // We failed to read a temperature :(
                    //
                    // This could be because someone unplugged the transceiver
                    // at exactly the right time, in which case, the error will
                    // be transient (and we'll remove the transceiver on the
                    // next pass through this function).
                    ringbuf_entry!(Trace::TemperatureReadError(i, e));
                }
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

impl idl::InOrderTransceiversImpl for ServerImpl {
    fn get_modules_status(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ModulesStatus, idol_runtime::RequestError<TransceiversError>>
    {
        Ok(self
            .transceivers
            .get_modules_status()
            .map_err(TransceiversError::from)?)
    }

    fn all_power_states(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<PowerStatesAll, idol_runtime::RequestError<TransceiversError>>
    {
        Ok(self
            .transceivers
            .all_power_states()
            .map_err(TransceiversError::from)?)
    }

    fn set_power_state(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
        state: PowerState,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .set_power_state(state, LogicalPortMask(logical_port_mask))
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn port_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .port_reset(LogicalPortMask(logical_port_mask))
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn port_clear_fault(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .port_clear_fault(LogicalPortMask(logical_port_mask))
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn setup_i2c_read(
        &mut self,
        _msg: &userlib::RecvMessage,
        reg: u8,
        num_bytes: u8,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if usize::from(num_bytes) > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        self.transceivers
            .setup_i2c_read(reg, num_bytes, LogicalPortMask(logical_port_mask))
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn setup_i2c_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        reg: u8,
        num_bytes: u8,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if usize::from(num_bytes) > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        self.transceivers
            .setup_i2c_write(reg, num_bytes, LogicalPortMask(logical_port_mask))
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn get_i2c_read_buffer(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port: u8,
        dest: Leased<W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if logical_port >= NUM_PORTS {
            return Err(TransceiversError::InvalidPortNumber.into());
        }
        let port = LogicalPort(logical_port);

        if dest.len() > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        let mut buf = [0u8; PAGE_SIZE_BYTES];

        self.transceivers
            .get_i2c_read_buffer(port, &mut buf[..dest.len()])
            .map_err(TransceiversError::from)?;

        dest.write_range(0..dest.len(), &buf[..dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        Ok(())
    }

    fn set_i2c_write_buffer(
        &mut self,
        _msg: &userlib::RecvMessage,
        data: Leased<R, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if data.len() > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        let mut buf = [0u8; PAGE_SIZE_BYTES];

        data.read_range(0..data.len(), &mut buf[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        self.transceivers
            .set_i2c_write_buffer(&buf[..data.len()])
            .map_err(TransceiversError::from)?;
        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK | notifications::SOCKET_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if (bits & notifications::SOCKET_MASK) != 0 {
            // Nothing to do here; we'll handle it in the main loop
        }

        if (bits & notifications::TIMER_MASK) != 0 {
            // Check for errors
            if self.leds_initialized {
                let errors = self.leds.error_summary().unwrap();
                if errors != self.led_error {
                    self.led_error = errors;
                    ringbuf_entry!(Trace::LEDErrorSummary(errors));
                }
            } else {
                ringbuf_entry!(Trace::LEDUninitialized);
            }

            // Query module presence and update LEDs accordingly
            let status = match self.transceivers.get_modules_status() {
                Ok(status) => status,
                Err(_) => ModulesStatus::new_zeroed(),
            };

            if status.present != self.modules_present {
                self.led_update(status.present);

                self.modules_present = status.present;
                ringbuf_entry!(Trace::ModulePresenceUpdate(status.present));
            }

            self.update_thermal_loop(status);

            let next_deadline = sys_get_timer().now + TIMER_INTERVAL;
            sys_set_timer(Some(next_deadline), notifications::TIMER_MASK);
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    loop {
        // This is a temporary workaround that makes sure the FPGAs are up
        // before we start doing things with them. A more sophisticated
        // notification system will be put in place.
        let seq = Sequencer::from(SEQ.get_task_id());
        loop {
            let ready = seq.front_io_phy_ready();
            match ready {
                Ok(true) => {
                    ringbuf_entry!(Trace::FrontIOReady(true));
                    break;
                }
                Err(SeqError::NoFrontIOBoard) => {
                    ringbuf_entry!(Trace::FrontIOSeqErr(
                        SeqError::NoFrontIOBoard
                    ));
                    break;
                }
                _ => {
                    ringbuf_entry!(Trace::FrontIOReady(false));
                    userlib::hl::sleep_for(10)
                }
            }
        }

        let transceivers = Transceivers::new(FRONT_IO.get_task_id());
        let leds = Leds::new(
            &i2c_config::devices::pca9956b_front_leds_left(I2C.get_task_id()),
            &i2c_config::devices::pca9956b_front_leds_right(I2C.get_task_id()),
        );

        let net = task_net_api::Net::from(NET.get_task_id());
        let thermal_api = Thermal::from(THERMAL.get_task_id());
        let sensor_api = Sensor::from(SENSOR.get_task_id());
        let (tx_data_buf, rx_data_buf) = claim_statics();
        let mut server = ServerImpl {
            transceivers,
            leds,
            net,
            modules_present: 0,
            led_error: Default::default(),
            leds_initialized: false,
            thermal_api,
            sensor_api,
            thermal_models: [None; NUM_PORTS as usize],
        };

        ringbuf_entry!(Trace::LEDInit);

        server.transceivers.enable_led_controllers().unwrap();
        server.led_init();

        // This will put our timer in the past, immediately forcing an update
        let deadline = sys_get_timer().now;
        sys_set_timer(Some(deadline), notifications::TIMER_MASK);

        let mut buffer = [0; idl::INCOMING_SIZE];
        loop {
            server.check_net(
                tx_data_buf.as_mut_slice(),
                rx_data_buf.as_mut_slice(),
            );
            idol_runtime::dispatch_n(&mut buffer, &mut server);
        }
    }
}
////////////////////////////////////////////////////////////////////////////////

/// Grabs references to the static descriptor/buffer receive rings. Can only be
/// called once.
pub fn claim_statics() -> (
    &'static mut [u8; MAX_UDP_MESSAGE_SIZE],
    &'static mut [u8; MAX_UDP_MESSAGE_SIZE],
) {
    const S: usize = MAX_UDP_MESSAGE_SIZE;
    mutable_statics::mutable_statics! {
        static mut TX_BUF: [u8; S] = [|| 0u8; _];
        static mut RX_BUF: [u8; S] = [|| 0u8; _];
    }
}
////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::{ModulesStatus, PowerState, PowerStatesAll, TransceiversError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
