// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SPD control task for Cosmo

#![no_std]
#![no_main]

use drv_cpu_seq_api::PowerState;
use drv_spartan7_loader_api::Spartan7Loader;
use idol_runtime::RequestError;
use ringbuf::{counted_ringbuf, ringbuf_entry};
use task_jefe_api::Jefe;
use task_packrat_api::Packrat;
use task_sensor_api::{config::other_sensors, NoData, Sensor, SensorId};
use userlib::{
    hl::sleep_for, set_timer_relative, sys_get_timer, task_slot, FromPrimitive,
    RecvMessage,
};
use zerocopy::IntoBytes;

task_slot!(JEFE, jefe);
task_slot!(PACKRAT, packrat);
task_slot!(LOADER, spartan7_loader);
task_slot!(SENSOR, sensor);

#[derive(counters::Count, Copy, Clone, PartialEq)]
enum Trace {
    None,
    Activate { time: u64 },
    Deactivate { time: u64 },
    Present { index: usize, present: bool },
    TemperatureReadTimeout { index: usize, pos: usize },
}

counted_ringbuf!(Trace, 32, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let packrat = Packrat::from(PACKRAT.get_task_id());
    let loader = Spartan7Loader::from(LOADER.get_task_id());
    let token = loader.get_token();
    let dimms = fmc_periph::Dimms::new(token);
    let sensor = Sensor::from(SENSOR.get_task_id());
    let jefe = Jefe::from(JEFE.get_task_id());

    let mut server = ServerImpl {
        dimms,
        sensor,
        present: [false; DIMM_COUNT],
        jefe,
        packrat,
        active: false,
    };

    // Wait for entry to A0 before we enable our i2c controller.  Normally,
    // we'd be able to read SPDs in A2, but there's a hardware errata:
    // https://github.com/oxidecomputer/hardware-cosmo/issues/689
    server.update_state();

    set_timer_relative(0, notifications::TIMER_MASK);
    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

// Poll the thermal sensors at roughly 4 Hz
const TIMER_INTERVAL: u32 = 250;
const DIMM_COUNT: usize = 12;

struct ServerImpl {
    dimms: fmc_periph::Dimms,
    sensor: Sensor,
    jefe: Jefe,
    packrat: Packrat,
    present: [bool; DIMM_COUNT],
    active: bool,
}

impl ServerImpl {
    /// Get our current state from `jefe` and activate / deactivate as needed
    fn update_state(&mut self) {
        // This laborious list is intended to ensure that new power states
        // have to be added explicitly here.
        match PowerState::from_u32(self.jefe.get_state()) {
            Some(PowerState::A0) | Some(PowerState::A0PlusHP) => {
                if !self.active {
                    self.activate()
                }
            }
            Some(PowerState::A2)
            | Some(PowerState::A2PlusFans)
            | Some(PowerState::A0Reset)
            | Some(PowerState::A0Thermtrip)
            | None => {
                if self.active {
                    self.deactivate()
                }
            }
        }
    }

    fn activate(&mut self) {
        ringbuf_entry!(Trace::Activate {
            time: sys_get_timer().now
        });

        // Kick off a read then wait for it to complete
        self.dimms.spd_ctrl.modify(|s| s.set_start(true));
        while self.dimms.spd_ctrl.start() {
            sleep_for(10);
        }

        for (index, present) in self.present.iter_mut().enumerate() {
            // Check if this channel is present
            *present = match index {
                0 => self.dimms.spd_present.bus0_a(),
                1 => self.dimms.spd_present.bus0_b(),
                2 => self.dimms.spd_present.bus0_c(),
                3 => self.dimms.spd_present.bus0_d(),
                4 => self.dimms.spd_present.bus0_e(),
                5 => self.dimms.spd_present.bus0_f(),
                6 => self.dimms.spd_present.bus1_g(),
                7 => self.dimms.spd_present.bus1_h(),
                8 => self.dimms.spd_present.bus1_i(),
                9 => self.dimms.spd_present.bus1_j(),
                10 => self.dimms.spd_present.bus1_k(),
                11 => self.dimms.spd_present.bus1_l(),
                DIMM_COUNT.. => unreachable!(),
            };
            ringbuf_entry!(Trace::Present {
                index,
                present: *present
            });
            if !*present {
                self.packrat.remove_spd(index as u8);
                continue;
            }
            // Set this channel as selected, clearing other selections
            self.dimms.spd_select.modify(|s| {
                s.set_bus0_a(false);
                s.set_bus0_b(false);
                s.set_bus0_c(false);
                s.set_bus0_d(false);
                s.set_bus0_e(false);
                s.set_bus0_f(false);
                s.set_bus1_g(false);
                s.set_bus1_h(false);
                s.set_bus1_i(false);
                s.set_bus1_j(false);
                s.set_bus1_k(false);
                s.set_bus1_l(false);
                match index {
                    0 => s.set_bus0_a(true),
                    1 => s.set_bus0_b(true),
                    2 => s.set_bus0_c(true),
                    3 => s.set_bus0_d(true),
                    4 => s.set_bus0_e(true),
                    5 => s.set_bus0_f(true),
                    6 => s.set_bus1_g(true),
                    7 => s.set_bus1_h(true),
                    8 => s.set_bus1_i(true),
                    9 => s.set_bus1_j(true),
                    10 => s.set_bus1_k(true),
                    11 => s.set_bus1_l(true),
                    DIMM_COUNT.. => unreachable!(),
                }
            });

            // Read 4x256 bytes from the FPGA's buffer and copy to Packrat
            self.dimms.spd_rd_ptr.set_addr(0);
            for i in 0..4 {
                // Limited by max lease size for Packrat
                let mut buf = [0u32; 64];
                for b in &mut buf {
                    *b = self.dimms.spd_rdata.data();
                }
                self.packrat.set_spd_eeprom(
                    index as u8,
                    i * 256,
                    buf.as_bytes(),
                );
            }
        }
        self.active = true;
    }

    fn deactivate(&mut self) {
        ringbuf_entry!(Trace::Deactivate {
            time: sys_get_timer().now
        });
        self.active = false;

        // Mark all sensors as off
        for dimm in &DIMM_SENSORS {
            for sensor in dimm {
                self.sensor.nodata_now(*sensor, NoData::DeviceOff);
            }
        }
    }

    fn poll_sensors(&mut self) {
        // The FPGA register generation produces different types for bus0 and
        // bus1, but they're the same shape, so we'll use a small macro for
        // codegen.
        macro_rules! dimm_read_temperature {
            ($addr:ident, $cmd:ident, $count:ident, $data:ident) => {{
                self.dimms.$cmd.modify(|b| {
                    b.set_bus_addr($addr);
                    b.set_len(2);
                    b.set_reg_addr(0x31); // current sensed temperature
                    b.set_op(2); // RANDOM_READ
                });
                const TIMEOUT_COUNT: usize = 8;
                let mut timed_out = false;
                for i in 0.. {
                    if self.dimms.$count.data() == 2 {
                        break;
                    } else if i == TIMEOUT_COUNT {
                        timed_out = true;
                        break;
                    }
                    sleep_for(1);
                }
                if timed_out {
                    None
                } else {
                    Some(self.dimms.$data.data())
                }
            }};
        }

        for (index, present) in self.present.iter().cloned().enumerate() {
            let bus = index / 6; // FPGA bus (0 or 1)
            let dev = index % 6; // device index (SDI, 0-6)

            for pos in 0..2 {
                // Mark sensors as absent if they're missing
                if !present {
                    self.sensor.nodata_now(
                        DIMM_SENSORS[index][pos],
                        NoData::DeviceNotPresent,
                    );
                    continue;
                }

                // See JESD302-1A for details on this address
                #[allow(clippy::unusual_byte_groupings)]
                let addr = (0b0010_000 | (pos << 5) | dev) as u8;
                let raw_temp = if bus == 0 {
                    dimm_read_temperature!(
                        addr,
                        bus0_cmd,
                        bus0_rx_byte_count,
                        bus0_rx_rdata
                    )
                } else {
                    dimm_read_temperature!(
                        addr,
                        bus1_cmd,
                        bus1_rx_byte_count,
                        bus1_rx_rdata
                    )
                };
                let Some(raw_temp) = raw_temp else {
                    ringbuf_entry!(Trace::TemperatureReadTimeout {
                        index,
                        pos,
                    });
                    self.sensor.nodata_now(
                        DIMM_SENSORS[index][pos],
                        NoData::DeviceTimeout,
                    );
                    continue;
                };

                // The actual temperature is a 13-bit two's complement value
                // (with two low bits reserved as 0s)
                //
                // We shift it so that the sign bit is in the right place,
                // cast it to an i16 to make it signed, then scale it into a
                // float.
                let t = (raw_temp << 3) as i16;
                let temp_c = f32::from(t) * 0.0078125f32;

                // Send the value to the sensors task
                self.sensor.post_now(DIMM_SENSORS[index][pos], temp_c);
            }
        }
    }
}

impl idl::InOrderCosmoSpdImpl for ServerImpl {
    fn ping(
        &mut self,
        _mgs: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        Ok(())
    }
}

const DIMM_SENSORS: [[SensorId; 2]; DIMM_COUNT] = [
    [
        other_sensors::DIMM_A_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_A_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_B_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_B_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_C_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_C_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_D_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_D_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_E_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_E_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_F_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_F_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_G_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_G_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_H_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_H_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_I_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_I_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_J_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_J_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_K_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_K_TS1_TEMPERATURE_SENSOR,
    ],
    [
        other_sensors::DIMM_L_TS0_TEMPERATURE_SENSOR,
        other_sensors::DIMM_L_TS1_TEMPERATURE_SENSOR,
    ],
];

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        if self.active {
            notifications::TIMER_MASK | notifications::JEFE_STATE_CHANGE_MASK
        } else {
            notifications::JEFE_STATE_CHANGE_MASK
        }
    }

    fn handle_notification(&mut self, bits: u32) {
        if (bits & notifications::JEFE_STATE_CHANGE_MASK) != 0 {
            self.update_state();
        }

        if self.active {
            if (bits & notifications::TIMER_MASK) != 0 {
                self.poll_sensors();
            }
            set_timer_relative(TIMER_INTERVAL, notifications::TIMER_MASK);
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

mod fmc_periph {
    include!(concat!(env!("OUT_DIR"), "/fmc_periph.rs"));
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
