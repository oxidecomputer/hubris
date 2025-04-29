// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Based on the `gimlet_bcdef.rs` implementation in this folder
use crate::{
    i2c_config, i2c_config::sensors, Device, PowerControllerConfig, PowerState,
    SensorId,
};

use drv_i2c_devices::max5970::*;
use ringbuf::*;
use userlib::units::*;

pub(crate) const CONTROLLER_CONFIG_LEN: usize = 43;
const MAX5970_CONFIG_LEN: usize = 22;

pub(crate) static CONTROLLER_CONFIG: [PowerControllerConfig;
    CONTROLLER_CONFIG_LEN] = [
    rail_controller!(IBC, bmr491, v12_sys_a2, A2),
    rail_controller!(Core, raa229620A, vddcr_cpu0_a0, A0),
    rail_controller!(Core, raa229620A, vddcr_soc_a0, A0),
    rail_controller!(Core, raa229620A, vddcr_cpu1_a0, A0),
    rail_controller!(Core, raa229620A, vddio_sp5_a0, A0),
    rail_controller_notemp!(Core, isl68224, v1p1_sp5_a0, A0),
    rail_controller_notemp!(Core, isl68224, v1p8_sp5_a1, A0), // XXX A0 or A2?
    rail_controller_notemp!(Core, isl68224, v3p3_sp5_a1, A0), // XXX A0 or A2?
    rail_controller!(Sys, tps546B24A, v3p3_sp_a2, A2),
    rail_controller!(Sys, tps546B24A, v5_sys_a2, A2),
    rail_controller!(Sys, tps546B24A, v1p8_sys_a2, A2),
    rail_controller!(Sys, tps546B24A, v0p96_nic_vdd_a0hp, A0),
    adm1272_controller!(HotSwap, v54p5_ibc_a3, A2, Ohms(0.000_750)),
    lm5066_controller!(
        Fan,
        v54p5_fan_east,
        A2,
        Ohms(0.007),
        drv_i2c_devices::lm5066::CurrentLimitStrap::VDD
    ),
    lm5066_controller!(
        Fan,
        v54p5_fan_central,
        A2,
        Ohms(0.007),
        drv_i2c_devices::lm5066::CurrentLimitStrap::VDD
    ),
    lm5066_controller!(
        Fan,
        v54p5_fan_west,
        A2,
        Ohms(0.007),
        drv_i2c_devices::lm5066::CurrentLimitStrap::VDD
    ),
    max5970_controller!(HotSwapIO, v3p3_m2a_a0hp, A0, Ohms(0.003)),
    max5970_controller!(HotSwapIO, v3p3_m2b_a0hp, A0, Ohms(0.003)),
    max5970_controller!(HotSwapIO, v12_u2a_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2a_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v12_u2b_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2b_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v12_u2c_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2c_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v12_u2d_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2d_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v12_u2e_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2e_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v12_u2f_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2f_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v12_u2g_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2g_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v12_u2h_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2h_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v12_u2i_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2i_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v12_u2j_a0, A0, Ohms(0.005)),
    max5970_controller!(HotSwapIO, v3p3_u2j_a0, A0, Ohms(0.005)),
    ltc4282_controller!(HotSwapIO, v12_mcio_a0hp, A0, Ohms(0.001)),
    ltc4282_controller!(HotSwapIO, v12_ddr5_abcdef_a0, A0, Ohms(0.001)),
    ltc4282_controller!(HotSwapIO, v12_ddr5_ghijkl_a0, A0, Ohms(0.001)),
    max5970_controller!(HotSwapIO, v12p0_nic_a0hp, A0, Ohms(0.003)),
    max5970_controller!(HotSwapIO, v5p0_nic_a0hp, A0, Ohms(0.003)),
];

pub(crate) fn get_state() -> PowerState {
    userlib::task_slot!(SEQUENCER, cosmo_seq);

    use drv_cpu_seq_api as seq_api;

    let sequencer = seq_api::Sequencer::from(SEQUENCER.get_task_id());

    //
    // We deliberately enumerate all power states to force the addition of
    // new ones to update this code.
    //
    match sequencer.get_state() {
        seq_api::PowerState::A0
        | seq_api::PowerState::A0PlusHP
        | seq_api::PowerState::A0Thermtrip
        | seq_api::PowerState::A0Reset => PowerState::A0,
        seq_api::PowerState::A1
        | seq_api::PowerState::A2
        | seq_api::PowerState::A2PlusFans => PowerState::A2,
    }
}

/// Number of seconds (really, timer firings) between writes to the trace
/// buffer.
const TRACE_SECONDS: u32 = 10;

/// Number of trace records to store.
///
/// TODO: explain rationale for this value.
const TRACE_DEPTH: usize = 52;

/// This enum and its corresponding ringbuf are being used to attempt to isolate
/// cases of this bug:
///
///     https://github.com/oxidecomputer/mfg-quality/issues/140
///
/// Unless that bug report is closed or says otherwise, be careful modifying
/// this type, as you may break data collection.
///
/// The basic theory here is:
///
/// - Every `TRACE_SECONDS` seconds, the task wakes up and writes one `Now`
///   entry.
///
/// - We then record one `Max5970` trace entry per MAX5970 being monitored.
///
/// Tooling can then collect this ringbuf periodically and get recent events.
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    /// Configuration of the MAX5970 failed
    Max5970ConfigFailed {
        u2_index: usize,
        err: drv_i2c_api::ResponseCode,
    },

    /// Written before trace records; the `u32` is the number of times the task
    /// has woken up to process its timer. This is not exactly equivalent to
    /// seconds because of the way the timer is maintained, but is approximately
    /// seconds.
    Now(u32),

    /// Trace record written for each MAX5970.
    ///
    /// The `last_bounce_detected` field and those starting with `crossbounce_`
    /// are copied from running state and may not be updated on every trace
    /// event. The other fields are read while emitting the trace record and
    /// should be current.
    Max5970 {
        sensor: SensorId,
        last_bounce_detected: Option<u32>,
        status0: u8,
        status1: u8,
        status3: u8,
        fault0: u8,
        fault1: u8,
        fault2: u8,
        min_iout: f32,
        max_iout: f32,
        min_vout: f32,
        max_vout: f32,
        crossbounce_min_iout: f32,
        crossbounce_max_iout: f32,
        crossbounce_min_vout: f32,
        crossbounce_max_vout: f32,
    },
    None,
}

ringbuf!(Trace, TRACE_DEPTH, Trace::None);

/// Records fields from `dev` and merges them with previous state from `peaks`,
/// updating `peaks` in the process.
///
/// If any I2C operation fails, this will abort its work and return.
fn trace_max5970(
    dev: &Max5970,
    sensor: SensorId,
    peaks: &mut Max5970Peaks,
    now: u32,
) {
    let max_vout = match dev.max_vout() {
        Ok(Volts(v)) => v,
        _ => return,
    };

    let min_vout = match dev.min_vout() {
        Ok(Volts(v)) => v,
        _ => return,
    };

    let max_iout = match dev.max_iout() {
        Ok(Amperes(a)) => a,
        _ => return,
    };

    let min_iout = match dev.min_iout() {
        Ok(Amperes(a)) => a,
        _ => return,
    };

    // TODO: this update should probably happen after all I/O is done.
    if peaks.iout.bounced(min_iout, max_iout)
        || peaks.vout.bounced(min_vout, max_vout)
    {
        peaks.last_bounce_detected = Some(now);
    }

    ringbuf_entry!(Trace::Max5970 {
        sensor,
        last_bounce_detected: peaks.last_bounce_detected,
        status0: match dev.read_reg(Register::status0) {
            Ok(reg) => reg,
            _ => return,
        },
        status1: match dev.read_reg(Register::status1) {
            Ok(reg) => reg,
            _ => return,
        },
        status3: match dev.read_reg(Register::status3) {
            Ok(reg) => reg,
            _ => return,
        },
        fault0: match dev.read_reg(Register::fault0) {
            Ok(reg) => reg,
            _ => return,
        },
        fault1: match dev.read_reg(Register::fault1) {
            Ok(reg) => reg,
            _ => return,
        },
        fault2: match dev.read_reg(Register::fault2) {
            Ok(reg) => reg,
            _ => return,
        },
        min_iout,
        max_iout,
        min_vout,
        max_vout,
        crossbounce_min_iout: peaks.iout.crossbounce_min,
        crossbounce_max_iout: peaks.iout.crossbounce_max,
        crossbounce_min_vout: peaks.vout.crossbounce_min,
        crossbounce_max_vout: peaks.vout.crossbounce_max,
    });
}

#[derive(Copy, Clone)]
struct Max5970Peak {
    min: f32,
    max: f32,
    crossbounce_min: f32,
    crossbounce_max: f32,
}

impl Default for Max5970Peak {
    fn default() -> Self {
        Self {
            min: f32::MAX,
            max: f32::MIN,
            crossbounce_min: f32::MAX,
            crossbounce_max: f32::MIN,
        }
    }
}

impl Max5970Peak {
    ///
    /// If we see the drives lose power, it is helpful to disambiguate PDN issues
    /// from the power being explicitly disabled via system software (e.g., via
    /// CEM_TO_PCIEHP_PWREN on Sharkfin).  The MAX5970 doesn't have a way of
    /// recording power cycles, but we know that if we see the peaks travel in
    /// the wrong direction (that is, a max that is less than the previous max
    /// or a minimum that is greater than our previous minimum) then there must
    /// have been a power cycle.  This can clearly yield false negatives, but
    /// it will not yield false positives:  if [`bounced`] returns true, one can
    /// know with confidence that the power has been cycled.  Note that we also
    /// use this opportunity to retain the peaks across a bounce, which would
    /// would otherwise be lost.
    ///
    fn bounced(&mut self, min: f32, max: f32) -> bool {
        let bounced = min > self.min || max < self.max;
        self.min = min;
        self.max = max;

        if min < self.crossbounce_min {
            self.crossbounce_min = min;
        }

        if max > self.crossbounce_max {
            self.crossbounce_max = max;
        }

        bounced
    }
}

#[derive(Copy, Clone, Default)]
struct Max5970Peaks {
    iout: Max5970Peak,
    vout: Max5970Peak,
    last_bounce_detected: Option<u32>,
}

pub(crate) struct State {
    fired: u32,
    peaks: [Max5970Peaks; MAX5970_CONFIG_LEN],
}

impl State {
    pub(crate) fn init() -> Self {
        // Tweak the thresholds for the U.2 MAX5970 12V outputs
        //
        // Current Sense Range: 50 mV (set by a resistor on the board)
        // Desired Fast Trip Current: 6A (DAC_CH0: 0x99)
        // Desired Fast-to-Slow Ratio: 200% (default value)
        // Resulting Slow Trip Current: 3A
        let i2c_task = super::I2C.get_task_id();

        for (i, builder) in [
            i2c_config::pmbus::v12_u2a_a0,
            i2c_config::pmbus::v12_u2b_a0,
            i2c_config::pmbus::v12_u2c_a0,
            i2c_config::pmbus::v12_u2d_a0,
            i2c_config::pmbus::v12_u2e_a0,
            i2c_config::pmbus::v12_u2f_a0,
            i2c_config::pmbus::v12_u2g_a0,
            i2c_config::pmbus::v12_u2h_a0,
            i2c_config::pmbus::v12_u2i_a0,
            i2c_config::pmbus::v12_u2j_a0,
        ]
        .iter()
        .enumerate()
        {
            let (dev, rail) = (builder)(i2c_task);
            let m = Max5970::new(&dev, rail, Ohms(0.005));
            if let Err(err) = m.set_dac_fast(0x99) {
                ringbuf_entry!(Trace::Max5970ConfigFailed { u2_index: i, err });
            }
        }

        Self {
            fired: 0,
            peaks: [Max5970Peaks::default(); MAX5970_CONFIG_LEN],
        }
    }

    pub(crate) fn handle_timer_fired(
        &mut self,
        devices: &[Device],
        state: PowerState,
    ) {
        //
        // Trace the detailed state every ten seconds, provided that we are in A0.
        //
        if state == PowerState::A0 && self.fired % TRACE_SECONDS == 0 {
            ringbuf_entry!(Trace::Now(self.fired));

            for ((dev, sensor), peak) in CONTROLLER_CONFIG
                .iter()
                .zip(devices.iter())
                .filter_map(|(c, dev)| {
                    if let Device::Max5970(dev) = dev {
                        Some((dev, c.current))
                    } else {
                        None
                    }
                })
                .zip(self.peaks.iter_mut())
            {
                trace_max5970(dev, sensor, peak, self.fired);
            }
        }

        self.fired += 1;
    }
}

pub const HAS_RENDMP_BLACKBOX: bool = true;
