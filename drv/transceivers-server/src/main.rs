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
    ModuleStatus, TransceiversError, NUM_PORTS, TRANSCEIVER_TEMPERATURE_SENSORS,
};
use enum_map::Enum;
use idol_runtime::{NotificationHandler, RequestError};
use multitimer::{Multitimer, Repeat};
use ringbuf::*;
use task_sensor_api::{NoData, Sensor, SensorApiError};
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
    FrontIOBoardReady(bool),
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
    RemovedDisabledModuleThermalModel(usize),
    TemperatureReadError(usize, Reg::QSFP::PORT0_STATUS::Encoded),
    TemperatureReadUnexpectedError(usize, FpgaError),
    SensorError(usize, SensorApiError),
    ThermalError(usize, ThermalError),
    GetInterfaceError(usize, Reg::QSFP::PORT0_STATUS::Encoded),
    GetInterfaceUnexpectedError(usize, FpgaError),
    InvalidPortStatusError(usize, u8),
    DisablingPorts(LogicalPortMask),
    DisableFailed(usize, LogicalPortMask),
    ClearDisabledPorts(LogicalPortMask),
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

/// After seeing this many NACKs, we disable the port by policy.
///
/// This should be **very rare**: it requires a transceiver to correctly report
/// its type (SFF vs CMIS) over I2C when it's first plugged in, but then begin
/// NACKing while still physically present (according to the `modprsl` pin).
///
/// Despite the weirdness of these pre-requisites, we've seen this happen once
/// already; without handling it, the thermal loop will eventually shut down the
/// whole system (because the transceiver stops reporting its temperature).
const MAX_CONSECUTIVE_NACKS: u8 = 3;

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

    /// Modules that are physically present but disabled by Hubris
    disabled: LogicalPortMask,

    /// Number of consecutive NACKS seen on a given port
    consecutive_nacks: [u8; NUM_PORTS as usize],

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
const SPI_INTERVAL: u64 = 500;

/// Controls how often we update the LED controllers (in milliseconds).
const I2C_INTERVAL: u64 = 100;

/// Blink LEDs at a 50% duty cycle (in milliseconds)
const BLINK_INTERVAL: u64 = 500;

impl ServerImpl {
    fn led_init(&mut self) {
        match self.leds.initialize_current() {
            Ok(_) => {
                self.set_system_led_state(LedState::On);
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
        let system_led_on = match self.system_led_state {
            LedState::On => true,
            LedState::Blink => self.blink_on,
            LedState::Off => false,
        };
        if let Err(e) = self.leds.update_system_led_state(system_led_on) {
            ringbuf_entry!(Trace::LEDUpdateError(e));
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
            let mut got_nack = false;
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
                    use Reg::QSFP::PORT0_STATUS::Encoded;
                    match Encoded::from_u8(e) {
                        Some(val) => {
                            got_nack |= matches!(val, Encoded::I2cAddressNack);
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

            self.consecutive_nacks[i] = if got_nack {
                self.consecutive_nacks[i].saturating_add(1)
            } else {
                0
            };

            if self.consecutive_nacks[i] >= MAX_CONSECUTIVE_NACKS {
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
            Transceivers::assert_reset,
            Transceivers::deassert_lpmode,
            Transceivers::disable_power,
        ]
        .iter()
        .enumerate()
        {
            let err = f(&mut self.transceivers, mask).error();
            if !err.is_empty() {
                ringbuf_entry!(Trace::DisableFailed(step, err));
            }
        }
        self.disabled |= mask;
        // We don't modify self.thermal_models here; that's left to
        // `update_thermal_loop`, which is in charge of communicating with
        // the `sensors` and `thermal` tasks.
    }

    fn handle_i2c_loop(&mut self) {
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
    }

    fn handle_spi_loop(&mut self) {
        // Query module presence as this drives other state
        let (status, _) = self.transceivers.get_module_status();

        let modules_present = LogicalPortMask(!status.modprsl);
        if modules_present != self.modules_present {
            // check to see if any disabled ports had their modules removed and
            // allow their power to be turned on when a module is reinserted
            let disabled_ports_removed =
                self.modules_present & !modules_present & self.disabled;
            if !disabled_ports_removed.is_empty() {
                self.disabled &= !disabled_ports_removed;
                self.transceivers.enable_power(disabled_ports_removed);
                ringbuf_entry!(Trace::ClearDisabledPorts(
                    disabled_ports_removed
                ));
            }

            self.modules_present = modules_present;
            ringbuf_entry!(Trace::ModulePresenceUpdate(modules_present));
        }

        self.update_thermal_loop(status);
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
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::SOCKET_MASK | notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        // Nothing to do here; notifications are just to wake up this task, and
        // all of the actual work is handled in the main loop
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
            let ready = seq.front_io_board_ready();

            match ready {
                Ok(true) => {
                    ringbuf_entry!(Trace::FrontIOBoardReady(true));
                    break;
                }
                Err(SeqError::NoFrontIOBoard) => {
                    ringbuf_entry!(Trace::FrontIOSeqErr(
                        SeqError::NoFrontIOBoard
                    ));
                    break;
                }
                _ => {
                    ringbuf_entry!(Trace::FrontIOBoardReady(false));
                    userlib::hl::sleep_for(100)
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
            disabled: LogicalPortMask(0),
            consecutive_nacks: [0; NUM_PORTS as usize],
            thermal_api,
            sensor_api,
            thermal_models: [None; NUM_PORTS as usize],
        };

        ringbuf_entry!(Trace::LEDInit);

        match server.transceivers.enable_led_controllers() {
            Ok(_) => server.led_init(),
            Err(e) => ringbuf_entry!(Trace::LEDEnableError(e)),
        };

        // There are two timers, one for each communication bus:
        #[derive(Copy, Clone, Enum)]
        #[allow(clippy::upper_case_acronyms)]
        enum Timers {
            I2C,
            SPI,
            Blink,
        }
        let mut multitimer =
            Multitimer::<Timers>::new(notifications::TIMER_BIT);
        // Immediately fire each timer, then begin to service regularly
        let now = sys_get_timer().now;
        multitimer.set_timer(
            Timers::I2C,
            now,
            Some(Repeat::AfterDeadline(I2C_INTERVAL)),
        );
        multitimer.set_timer(
            Timers::SPI,
            now,
            Some(Repeat::AfterDeadline(SPI_INTERVAL)),
        );
        multitimer.set_timer(
            Timers::Blink,
            now,
            Some(Repeat::AfterDeadline(BLINK_INTERVAL)),
        );

        let mut buffer = [0; idl::INCOMING_SIZE];
        loop {
            multitimer.poll_now();
            for t in multitimer.iter_fired() {
                match t {
                    Timers::I2C => {
                        server.handle_i2c_loop();
                    }
                    Timers::SPI => {
                        server.handle_spi_loop();
                    }
                    Timers::Blink => {
                        server.blink_on = !server.blink_on;
                    }
                }
            }
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
