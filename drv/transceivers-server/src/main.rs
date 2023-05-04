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
    ModuleStatus, TransceiversError, NUM_PORTS, PAGE_SIZE_BYTES,
    TRANSCEIVER_TEMPERATURE_SENSORS,
};
use idol_runtime::{
    ClientError, Leased, NotificationHandler, RequestError, R, W,
};
use ringbuf::*;
use task_sensor_api::{NoData, Sensor, SensorError};
use task_thermal_api::{Thermal, ThermalError, ThermalProperties};
use transceiver_messages::{
    message::LedState, mgmt::ManagementInterface, MAX_PACKET_SIZE,
};
use userlib::{units::Celsius, *};
use zerocopy::{AsBytes, FromBytes};

mod udp; // UDP API is implemented in a separate file

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(SEQ, seq);
task_slot!(NET, net);
task_slot!(THERMAL, thermal);
task_slot!(SENSOR, sensor);

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
    LEDEnableError(FpgaError),
    LEDReadError(Error),
    LEDUpdateError(Error),
    ModulePresenceUpdate(LogicalPortMask),
    TransceiversError(TransceiversError),
    GotInterface(u8, ManagementInterface),
    UnknownInterface(u8, ManagementInterface),
    UnpluggedModule(usize),
    TemperatureReadError(usize, Reg::QSFP::PORT0_STATUS::Encoded),
    TemperatureReadUnexpectedError(usize, FpgaError),
    SensorError(usize, SensorError),
    ThermalError(usize, ThermalError),
    GetInterfaceError(usize, Reg::QSFP::PORT0_STATUS::Encoded),
    GetInterfaceUnexpectedError(usize, FpgaError),
    InvalidPortStatusError(usize, u8),
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone)]
struct LedStates([LedState; NUM_PORTS as usize]);

struct ServerImpl {
    transceivers: Transceivers,
    leds: Leds,
    net: task_net_api::Net,
    modules_present: LogicalPortMask,

    /// State around LED management
    led_error: FullErrorSummary,
    leds_initialized: bool,
    led_states: LedStates,
    blink_on: bool,
    system_led_state: LedState,

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
/// For transceivers that are present and include a thermal model, we measure
/// their temperature and send it to the `thermal` task.
const TIMER_INTERVAL: u64 = 500;

impl ServerImpl {
    fn led_init(&mut self) {
        match self
            .leds
            .initialize_current()
            .and(self.leds.update_system_led_state(true))
        {
            Ok(_) => {
                self.leds_initialized = true;
                ringbuf_entry!(Trace::LEDInitComplete);
            }
            Err(e) => ringbuf_entry!(Trace::LEDInitError(e)),
        };
    }

    fn set_led_state(&mut self, mask: LogicalPortMask, state: LedState) {
        for index in mask.to_indices() {
            self.led_states.0[index.0 as usize] = state;
        }
    }

    fn get_led_state(&self, port: LogicalPort) -> LedState {
        self.led_states.0[port.0 as usize]
    }

    fn set_system_led_state(&mut self, state: LedState) {
        self.system_led_state = state;
    }

    #[allow(dead_code)]
    fn get_system_led_state(&self) -> LedState {
        self.system_led_state
    }

    fn update_leds(&mut self) {
        // handle port LEDs
        let mut next_state = LogicalPortMask(0);
        for (i, state) in self.led_states.0.into_iter().enumerate() {
            let i = LogicalPort(i as u8);
            match state {
                LedState::On => next_state.set(i),
                LedState::Blink => {
                    if self.blink_on {
                        next_state.set(i)
                    }
                }
                LedState::Off => (),
            }
        }
        if let Err(e) = self.leds.update_led_state(next_state) {
            ringbuf_entry!(Trace::LEDUpdateError(e));
        }
        // handle system LED
        if let Err(e) = match self.system_led_state {
            LedState::On => self.leds.update_system_led_state(true),
            LedState::Blink => self.leds.update_system_led_state(self.blink_on),
            LedState::Off => self.leds.update_system_led_state(false),
        } {
            ringbuf_entry!(Trace::LEDUpdateError(e));
        }
        // keep track if we are on or off next update when blinking
        self.blink_on = !self.blink_on;
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
        let result = self.transceivers.setup_i2c_read(reg, 2, port.as_mask());
        if !result.error().is_empty() {
            return Err(FpgaError::CommsError);
        }

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
                    return Err(FpgaError::ImplError(out.status));
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
        let result = self.transceivers.setup_i2c_read(0, 1, port.as_mask());
        if !result.error().is_empty() {
            return Err(FpgaError::CommsError);
        }

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
            Err(FpgaError::ImplError(out[0]))
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
        for i in 0..self.thermal_models.len() {
            let port = LogicalPort(i as u8);
            let mask = 1 << i;
            let operational = (!status.modprsl & mask) != 0
                && (status.power_good & mask) != 0
                && (status.resetl & mask) != 0;

            // A wild transceiver just appeared!  Read it to decide whether it's
            // using SFF-8636 or CMIS.
            if operational && self.thermal_models[i].is_none() {
                match self.get_transceiver_interface(port) {
                    Ok(interface) => {
                        self.thermal_models[i] =
                            self.decode_interface(port, interface)
                    }
                    Err(FpgaError::ImplError(e)) => {
                        match Reg::QSFP::PORT0_STATUS::Encoded::from_u8(e) {
                            Some(val) => {
                                ringbuf_entry!(Trace::GetInterfaceError(i, val))
                            }
                            None => {
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
            // the thermal task still has it in case of restart.  This will
            // return a `NotInAutoMode` error if the thermal loop is in manual
            // mode; this is harmless and will be ignored (instead of cluttering
            // up the logs).
            match self.thermal_api.update_dynamic_input(i, m.model) {
                Ok(()) | Err(ThermalError::NotInAutoMode) => (),
                Err(e) => ringbuf_entry!(Trace::ThermalError(i, e)),
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
                // We failed to read a temperature :(
                //
                // This could be because someone unplugged the transceiver
                // at exactly the right time, in which case, the error will
                // be transient (and we'll remove the transceiver on the
                // next pass through this function).
                Err(FpgaError::ImplError(e)) => {
                    match Reg::QSFP::PORT0_STATUS::Encoded::from_u8(e) {
                        Some(val) => {
                            ringbuf_entry!(Trace::TemperatureReadError(i, val))
                        }
                        None => {
                            // Error code cannot be decoded
                            ringbuf_entry!(Trace::InvalidPortStatusError(i, e))
                        }
                    }
                }
                Err(e) => {
                    ringbuf_entry!(Trace::TemperatureReadUnexpectedError(i, e));
                }
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

impl idl::InOrderTransceiversImpl for ServerImpl {
    fn get_module_status(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ModuleStatus, idol_runtime::RequestError<TransceiversError>>
    {
        let (mod_status, result) = self.transceivers.get_module_status();
        if result.error().is_empty() {
            Ok(mod_status)
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
    }

    fn port_enable_power(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        let mask = LogicalPortMask(logical_port_mask);
        let result = self.transceivers.enable_power(mask);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
    }

    fn port_disable_power(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        let mask = LogicalPortMask(logical_port_mask);
        let result = self.transceivers.disable_power(mask);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
    }

    fn port_assert_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        let mask = LogicalPortMask(logical_port_mask);
        let result = self.transceivers.assert_reset(mask);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
    }

    fn port_deassert_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        let mask = LogicalPortMask(logical_port_mask);
        let result = self.transceivers.deassert_reset(mask);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
    }

    fn port_assert_lpmode(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        let mask = LogicalPortMask(logical_port_mask);
        let result = self.transceivers.assert_lpmode(mask);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
    }

    fn port_deassert_lpmode(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        let mask = LogicalPortMask(logical_port_mask);
        let result = self.transceivers.deassert_lpmode(mask);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
    }

    fn clear_power_fault(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        let mask = LogicalPortMask(logical_port_mask);
        let result = self.transceivers.clear_power_fault(mask);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
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
        let mask = LogicalPortMask(logical_port_mask);
        let result = self.transceivers.setup_i2c_read(reg, num_bytes, mask);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
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
        let mask = LogicalPortMask(logical_port_mask);
        let result = self.transceivers.setup_i2c_write(reg, num_bytes, mask);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
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

        let result = self.transceivers.set_i2c_write_buffer(&buf[..data.len()]);
        if result.error().is_empty() {
            Ok(())
        } else {
            Err(RequestError::from(TransceiversError::FpgaError))
        }
    }

    fn set_port_led_on(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.set_led_state(LogicalPortMask(logical_port_mask), LedState::On);
        Ok(())
    }

    fn set_port_led_off(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.set_led_state(LogicalPortMask(logical_port_mask), LedState::Off);
        Ok(())
    }

    fn set_port_led_blink(
        &mut self,
        _msg: &userlib::RecvMessage,
        logical_port_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.set_led_state(LogicalPortMask(logical_port_mask), LedState::Blink);
        Ok(())
    }

    fn set_system_led_on(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.set_system_led_state(LedState::On);
        Ok(())
    }

    fn set_system_led_off(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.set_system_led_state(LedState::Off);
        Ok(())
    }

    fn set_system_led_blink(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.set_system_led_state(LedState::Blink);
        Ok(())
    }

    fn set_led_current(
        &mut self,
        _msg: &userlib::RecvMessage,
        value: u8,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.leds
            .set_current(value)
            .map_err(|_| RequestError::from(TransceiversError::LedI2cError))
    }

    fn set_led_pwm(
        &mut self,
        _msg: &userlib::RecvMessage,
        value: u8,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.leds.set_pwm(value);
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
                self.update_leds();
                let errors = match self.leds.error_summary() {
                    Ok(errs) => errs,
                    Err(e) => {
                        ringbuf_entry!(Trace::LEDReadError(e));
                        Default::default()
                    }
                };
                if errors != self.led_error {
                    self.led_error = errors;
                    ringbuf_entry!(Trace::LEDErrorSummary(errors));
                }
            } else {
                ringbuf_entry!(Trace::LEDUninitialized);
            }

            // Query module presence and update LEDs accordingly
            let (status, _) = self.transceivers.get_module_status();

            let modules_present = LogicalPortMask(!status.modprsl);
            if modules_present != self.modules_present {
                self.modules_present = modules_present;
                ringbuf_entry!(Trace::ModulePresenceUpdate(modules_present));
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
            modules_present: LogicalPortMask(0),
            led_error: Default::default(),
            leds_initialized: false,
            led_states: LedStates([LedState::Off; NUM_PORTS as usize]),
            blink_on: false,
            system_led_state: LedState::Off,
            thermal_api,
            sensor_api,
            thermal_models: [None; NUM_PORTS as usize],
        };

        ringbuf_entry!(Trace::LEDInit);

        match server.transceivers.enable_led_controllers() {
            Ok(_) => server.led_init(),
            Err(e) => ringbuf_entry!(Trace::LEDEnableError(e)),
        };

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
    &'static mut [u8; MAX_PACKET_SIZE],
    &'static mut [u8; MAX_PACKET_SIZE],
) {
    const S: usize = MAX_PACKET_SIZE;
    mutable_statics::mutable_statics! {
        static mut TX_BUF: [u8; S] = [|| 0u8; _];
        static mut RX_BUF: [u8; S] = [|| 0u8; _];
    }
}
////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::{ModuleStatus, TransceiversError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
