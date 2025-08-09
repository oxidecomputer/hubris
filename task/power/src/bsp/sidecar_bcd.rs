// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    i2c_config::{self, sensors},
    Ohms, PowerControllerConfig, PowerState,
};

pub(crate) const CONTROLLER_CONFIG_LEN: usize = 16;
pub(crate) static CONTROLLER_CONFIG: [PowerControllerConfig;
    CONTROLLER_CONFIG_LEN] = [
    rail_controller!(IBC, bmr491, v12p0_sys, A2),
    adm1272_controller!(Fan, v54_fan0, A2, Ohms(0.001)),
    adm1272_controller!(Fan, v54_fan1, A2, Ohms(0.001)),
    adm1272_controller!(Fan, v54_fan2, A2, Ohms(0.001)),
    adm1272_controller!(Fan, v54_fan3, A2, Ohms(0.001)),
    adm1272_controller!(Fan, v54_hsc, A2, Ohms(0.001)),
    rail_controller!(Core, raa229618, v0p8_tf2_vdd_core, A0),
    rail_controller!(Sys, tps546B24A, v3p3_sys, A2),
    rail_controller!(Sys, tps546B24A, v5p0_sys, A2),
    rail_controller!(Core, raa229618, v1p5_tf2_vdda, A0),
    rail_controller!(Core, raa229618, v0p9_tf2_vddt, A0),
    rail_controller!(SerDes, isl68224, v1p8_tf2_vdda, A0),
    rail_controller!(SerDes, isl68224, v1p8_tf2_vdd, A0),
    rail_controller!(Sys, tps546B24A, v1p0_mgmt, A2),
    rail_controller!(Sys, tps546B24A, v1p8_sys, A2),
    ltc4282_controller!(HotSwapQSFP, v12p0_front_io, A2, Ohms(0.001 / 2.0)),
];

pub(crate) fn get_state() -> PowerState {
    userlib::task_slot!(SEQUENCER, sequencer);

    use drv_sidecar_seq_api as seq_api;

    let sequencer = seq_api::Sequencer::from(SEQUENCER.get_task_id());

    match sequencer.tofino_seq_state() {
        Ok(seq_api::TofinoSeqState::A0) => PowerState::A0,
        Ok(seq_api::TofinoSeqState::A2) => PowerState::A2,
        _ => {
            panic!("bad state");
        }
    }
}

pub(crate) struct State(());

impl State {
    pub(crate) fn init() -> Self {
        State(())
    }

    pub(crate) fn handle_timer_fired(
        &self,
        _devices: &[crate::Device],
        _state: PowerState,
        _packrat: &mut task_packrat_api::Packrat,
    ) {
    }
}

pub const HAS_RENDMP_BLACKBOX: bool = true;
