// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_stm32xx_sys_api::{
    self as sys_api, Mode, OutputType, Pull, Speed, Sys,
};

/// Helper struct to configure SP's RMII pins
pub struct RmiiPins {
    pub refclk: sys_api::PinSet,
    pub crs_dv: sys_api::PinSet,
    pub tx_en: sys_api::PinSet,
    pub txd1: sys_api::PinSet,
    pub txd0: sys_api::PinSet,
    pub rxd1: sys_api::PinSet,
    pub rxd0: sys_api::PinSet,

    pub af: sys_api::Alternate,
}
impl RmiiPins {
    pub fn configure(&self, sys: &Sys) {
        for p in &[
            self.refclk,
            self.crs_dv,
            self.tx_en,
            self.txd1,
            self.txd0,
            self.rxd1,
            self.rxd0,
        ] {
            sys.gpio_configure(
                p.port,
                p.pin_mask,
                Mode::Alternate,
                OutputType::PushPull,
                Speed::VeryHigh,
                Pull::None,
                self.af,
            );
        }
    }
}

/// Helper struct to configure MDIO pins
pub struct MdioPins {
    pub mdio: sys_api::PinSet,
    pub mdc: sys_api::PinSet,

    pub af: sys_api::Alternate,
}
impl MdioPins {
    pub fn configure(&self, sys: &Sys) {
        for p in &[self.mdio, self.mdc] {
            // Using Speed::Low because otherwise the VSC8504 refuses to talk
            sys.gpio_configure(
                p.port,
                p.pin_mask,
                Mode::Alternate,
                OutputType::PushPull,
                Speed::Low,
                Pull::None,
                self.af,
            );
        }
    }
}
