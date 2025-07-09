// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A crate for managing the power supplies on the Medusa board

use crate::*;
use drv_stm32xx_sys_api as sys_api;
use sys_api::{OutputType, Port, Pull, Speed, Sys};

task_slot!(SYS, sys);

pub struct PowerRail {
    /// The output GPIO for the power rail's enable pin
    enable: sys_api::PinSet,
    /// The input GPIO for the power rail's power good pin
    power_good: sys_api::PinSet,
    /// A RailName variant for ringbuf activity
    name: RailName,
}

impl PowerRail {
    pub fn new(
        enable: sys_api::PinSet,
        power_good: sys_api::PinSet,
        name: RailName,
    ) -> Self {
        let sys = Sys::from(SYS.get_task_id());

        sys.gpio_configure_output(
            enable,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        );

        sys.gpio_configure_input(power_good, Pull::None);

        Self {
            enable,
            power_good,
            name,
        }
    }

    /// Sets the enable pin for the power rail to HIGH if `enabled` is true or
    /// LOW if it is false.
    pub fn set_enable(&self, enabled: bool) {
        let sys = Sys::from(SYS.get_task_id());
        sys.gpio_set_to(self.enable, enabled);
        ringbuf_entry!(Trace::PowerEnable(self.name, enabled));
    }

    /// Returns the status of the power good pin. If power good is not HIGH this
    /// function disables the power rail automatically.
    pub fn check_power_good(&self) -> bool {
        if !self.power_good() {
            ringbuf_entry!(Trace::PowerFault(self.name));
            self.set_enable(false);
            return false;
        }
        true
    }

    /// Returns the status of the power good signal for the rail
    pub fn power_good(&self) -> bool {
        let sys = Sys::from(SYS.get_task_id());
        sys.gpio_read(self.power_good) != 0
    }
}

/// This struct contains PowerRails for most of the various power rails on Medusa. The exception is
/// the V12_QSFP_OUT rail which is controlled by the front-io server.
pub struct PowerControl {
    pub v1p0_mgmt: PowerRail,
    pub v1p2_mgmt: PowerRail,
    pub v2p5_mgmt: PowerRail,
    pub v1p0_front_phy: PowerRail,
    pub v2p5_front_phy: PowerRail,
    pub v1p0_local_phy: PowerRail,
    pub v2p5_local_phy: PowerRail,
}

impl PowerControl {
    pub fn new() -> Self {
        // VSC7448 rails
        let v1p0_mgmt =
            PowerRail::new(Port::J.pin(4), Port::J.pin(3), RailName::V1P0Mgmt);
        let v1p2_mgmt =
            PowerRail::new(Port::J.pin(6), Port::J.pin(5), RailName::V1P2Mgmt);
        let v2p5_mgmt =
            PowerRail::new(Port::J.pin(8), Port::J.pin(7), RailName::V2P5Mgmt);

        // The Front IO VSC8562 rails are generated from the same LDO which shares an
        // enable pin
        let v1p0_front_phy = PowerRail::new(
            Port::I.pin(11),
            Port::I.pin(12),
            RailName::V1P0FrontPhy,
        );
        let v2p5_front_phy = PowerRail::new(
            Port::I.pin(11),
            Port::I.pin(13),
            RailName::V2P5FrontPhy,
        );

        // The local VSC8562 rails are generated from the same LDO which shares an
        // enable pin
        let v1p0_local_phy = PowerRail::new(
            Port::J.pin(10),
            Port::J.pin(11),
            RailName::V1P0LocalPhy,
        );
        let v2p5_local_phy = PowerRail::new(
            Port::J.pin(10),
            Port::J.pin(12),
            RailName::V2P5LocalPhy,
        );

        Self {
            v1p0_mgmt,
            v1p2_mgmt,
            v2p5_mgmt,
            v1p0_front_phy,
            v2p5_front_phy,
            v1p0_local_phy,
            v2p5_local_phy,
        }
    }

    /// Returns true if all MGMT power rails are good. If that is not the case,
    /// disable all management rails and returns false.
    pub fn mgmt_power_check(&self) -> bool {
        let all_good = self.v1p0_mgmt.check_power_good()
            && self.v1p2_mgmt.check_power_good()
            && self.v2p5_mgmt.check_power_good();

        if !all_good {
            self.v1p0_mgmt.set_enable(false);
            self.v1p2_mgmt.set_enable(false);
            self.v2p5_mgmt.set_enable(false);
        }

        all_good
    }

    /// Returns true if both front PHY power rails are good. If that is not the case,
    /// disable all PHY rails and returns false.
    pub fn front_phy_power_check(&self) -> bool {
        let all_good = self.v1p0_front_phy.check_power_good()
            && self.v2p5_front_phy.check_power_good();

        if !all_good {
            self.v1p0_front_phy.set_enable(false);
            self.v2p5_front_phy.set_enable(false);
        }

        all_good
    }

    /// Returns true if both local PHY power rails are good. If that is not the case,
    /// disable all PHY rails and returns false.
    pub fn local_phy_power_check(&self) -> bool {
        let all_good = self.v1p0_local_phy.check_power_good()
            && self.v2p5_local_phy.check_power_good();

        if !all_good {
            self.v1p0_local_phy.set_enable(false);
            self.v2p5_local_phy.set_enable(false);
        }

        all_good
    }

    pub fn get_rail(&self, name: RailName) -> &PowerRail {
        use RailName::*;
        match name {
            V1P0Mgmt => &self.v1p0_mgmt,
            V1P2Mgmt => &self.v1p2_mgmt,
            V2P5Mgmt => &self.v2p5_mgmt,
            V1P0FrontPhy => &self.v1p0_front_phy,
            V2P5FrontPhy => &self.v2p5_front_phy,
            V1P0LocalPhy => &self.v1p0_local_phy,
            V2P5LocalPhy => &self.v2p5_local_phy,
        }
    }
}
