// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use counters::Count;
use idol_runtime::{NotificationHandler, RequestError};
use multitimer::{Multitimer, Repeat};
use ringbuf::*;
use static_cell::ClaimOnceCell;
use userlib::{sys_get_timer, task_slot, units::Celsius};

use drv_fpga_api::FpgaError;
use drv_i2c_devices::pca9956b::Error;
use drv_sidecar_front_io::{
    leds::{FullErrorSummary, Leds},
    transceivers::{
        FpgaI2CFailure, LogicalPort, LogicalPortMask, Transceivers,
    },
    Reg,
};
use drv_sidecar_seq_api::{SeqError, Sequencer, TofinoSeqState};
use drv_transceivers_api::{
    ModuleStatus, TransceiversError, NUM_PORTS, TRANSCEIVER_TEMPERATURE_SENSORS,
};
use enum_map::Enum;
use task_sensor_api::{NoData, Sensor};
#[allow(unused_imports)]
use task_thermal_api::{Thermal, ThermalError, ThermalProperties};
use transceiver_messages::{
    message::LedState, mgmt::ManagementInterface, MAX_PACKET_SIZE,
};

use zerocopy::{FromBytes, FromZeros, IntoBytes};

mod udp; // UDP API is implemented in a separate file

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(SEQ, seq);
task_slot!(NET, net);
task_slot!(SENSOR, sensor);

#[cfg(feature = "thermal-control")]
task_slot!(THERMAL, thermal);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    #[count(skip)]
    None,
    FrontIOBoardReady(#[count(children)] bool),
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
    SeqError(SeqError),
    ModuleTemperatureCritical(u8, Celsius),
    ModuleTemperaturePowerDown(u8, Celsius),
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

#[derive(Copy, Clone)]
struct LedStates([LedState; NUM_PORTS as usize]);

#[derive(Copy, Clone, PartialEq)]
enum FrontIOStatus {
    NotReady,
    NotPresent,
    Ready,
}

struct ServerImpl {
    transceivers: Transceivers,
    leds: Leds,
    net: task_net_api::Net,
    modules_present: LogicalPortMask,

    /// The Front IO board is not guaranteed to be present and ready
    front_io_board_present: FrontIOStatus,

    /// State around LED management
    led_error: FullErrorSummary,
    leds_initialized: bool,
    led_states: LedStates,
    blink_on: bool,
    system_led_state: LedState,

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

    fn update_leds(&mut self, seq_state: TofinoSeqState) {
        let mut next_state = LogicalPortMask(0);

        // We only turn transceiver LEDs on when Sidecar is in A0, since that is when there can be
        // meaningful link activity happening. When outside of A0, we default the LEDs to off.
        if seq_state == TofinoSeqState::A0 {
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

        #[derive(Copy, Clone, FromBytes, IntoBytes)]
        #[repr(C)]
        struct Temperature {
            temperature: zerocopy::I16<zerocopy::BigEndian>,
        }

        let mut out = Temperature::new_zeroed();
        let status = self
            .transceivers
            .get_i2c_status_and_read_buffer(port, out.as_mut_bytes())?;

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
        let result = self.transceivers.setup_i2c_read(0, 1, port.as_mask());
        if !result.error().is_empty() {
            return Err(FpgaError::CommsError);
        }

        let mut out = [0u8; 1]; // [SFF8024Identifier]

        // Wait for the I2C transaction to complete
        let status = self
            .transceivers
            .get_i2c_status_and_read_buffer(port, &mut out)?;

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
                        power_down_enabled: true,
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

                let model = ThermalProperties {
                    // We do *not* want the thermal loop to power down the whole
                    // system in response to a transceiver overheating. Instead,
                    // we will just disable the individual transceiver here.
                    // Thus, disable power-down on the version of the device's
                    // thermal properties we give to the `thermal` task.
                    power_down_enabled: false,
                    ..m.model
                };
                match self.thermal_api.update_dynamic_input(i, model) {
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

                    if m.model.should_power_down(t) {
                        // If the module's temperature exceeds the power-down
                        // threshold, add it to the list of things to disable.
                        ringbuf_entry!(Trace::ModuleTemperaturePowerDown(
                            port.0, t
                        ));
                        // TODO(eliza): ereport
                        // TODO(eliza): debounce
                        to_disable.set(port);
                    } else if m.model.is_critical(t) {
                        ringbuf_entry!(Trace::ModuleTemperatureCritical(
                            port.0, t
                        ));
                        // TODO(eliza): ereport
                        // TODO(eliza): track over critical duration...
                    }
                    // TODO(eliza): see if it's nominal again and turn it back
                    // on...?
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

    fn handle_i2c_loop(&mut self, seq_state: TofinoSeqState) {
        if self.leds_initialized {
            self.update_leds(seq_state);
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

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        // Nothing to do here; notifications are just to wake up this task, and
        // all of the actual work is handled in the main loop
    }
}

#[export_name = "main"]
fn main() -> ! {
    // This is a temporary workaround that makes sure the FPGAs are up
    // before we start doing things with them. A more sophisticated
    // notification system will be put in place.
    let seq = Sequencer::from(SEQ.get_task_id());

    let transceivers = Transceivers::new(FRONT_IO.get_task_id());
    let leds = Leds::new(
        &i2c_config::devices::pca9956b_front_leds_left(I2C.get_task_id()),
        &i2c_config::devices::pca9956b_front_leds_right(I2C.get_task_id()),
    );

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
        transceivers,
        leds,
        net,
        modules_present: LogicalPortMask(0),
        front_io_board_present: FrontIOStatus::NotReady,
        led_error: Default::default(),
        leds_initialized: false,
        led_states: LedStates([LedState::Off; NUM_PORTS as usize]),
        blink_on: false,
        system_led_state: LedState::Off,
        disabled: LogicalPortMask(0),
        consecutive_errors: [0; NUM_PORTS as usize],
        #[cfg(feature = "thermal-control")]
        thermal_api,
        sensor_api,
        thermal_models: [None; NUM_PORTS as usize],
    };

    // There are two timers, one for each communication bus:
    #[derive(Copy, Clone, Enum)]
    #[allow(clippy::upper_case_acronyms)]
    enum Timers {
        I2C,
        SPI,
        Blink,
    }
    let mut multitimer = Multitimer::<Timers>::new(notifications::TIMER_BIT);
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
        if server.front_io_board_present == FrontIOStatus::NotReady {
            server.front_io_board_present = match seq.front_io_board_ready() {
                Ok(true) => {
                    ringbuf_entry!(Trace::FrontIOBoardReady(true));
                    FrontIOStatus::Ready
                }
                Err(SeqError::NoFrontIOBoard) => {
                    ringbuf_entry!(Trace::FrontIOSeqErr(
                        SeqError::NoFrontIOBoard
                    ));
                    FrontIOStatus::NotPresent
                }
                _ => {
                    ringbuf_entry!(Trace::FrontIOBoardReady(false));
                    FrontIOStatus::NotReady
                }
            };

            // If a board is present, attempt to initialize its
            // LED drivers
            if server.front_io_board_present == FrontIOStatus::Ready {
                ringbuf_entry!(Trace::LEDInit);
                match server.transceivers.enable_led_controllers() {
                    Ok(_) => server.led_init(),
                    Err(e) => {
                        ringbuf_entry!(Trace::LEDEnableError(e))
                    }
                };
            }
        }

        multitimer.poll_now();
        for t in multitimer.iter_fired() {
            match t {
                Timers::I2C => {
                    // Check what power state we are in since that can impact LED state which is
                    // part of the I2C loop.
                    let seq_state =
                        seq.tofino_seq_state().unwrap_or_else(|e| {
                            // The failure path here is that we cannot get the state from the FPGA.
                            // If we cannot communicate with the FPGA then something has likely went
                            // rather wrong, and we are probably not in A0. For handling the error
                            // we will assume to be in the Init state, since that is what the main
                            // sequencer does as well.
                            ringbuf_entry!(Trace::SeqError(e));
                            TofinoSeqState::Init
                        });

                    // Handle the Front IO status checking as part of this
                    // loop because the frequency is what we had before and
                    // the server itself has no knowledge of the sequencer.
                    server.handle_i2c_loop(seq_state);
                }
                Timers::SPI => {
                    if server.front_io_board_present == FrontIOStatus::Ready {
                        server.handle_spi_loop();
                    }
                }
                Timers::Blink => {
                    server.blink_on = !server.blink_on;
                }
            }
        }

        server
            .check_net(tx_data_buf.as_mut_slice(), rx_data_buf.as_mut_slice());
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{ModuleStatus, TransceiversError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
