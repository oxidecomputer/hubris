// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{PSU_COUNT, i2c_config, notifications};
use drv_stm32xx_sys_api as sys_api;

pub use drv_i2c_devices::mwocp6x::Mwocp68 as Mwocp6x;

pub const STATUS_LED: sys_api::PinSet = sys_api::Port::A.pin(3);

// The ON signals are conveniently all routed to a single port:
pub const PSU_ENABLE_L_PORT: sys_api::Port = sys_api::Port::K;

// The ON signals are routed to the following pins on their port:
pub const PSU_ENABLE_L_PINS: [usize; PSU_COUNT] = [0, 1, 2, 3, 4, 5];

// Convenient mask for referring to all the ON pins simultaneously, since we can
// do that, since they're all on one port.
pub const ALL_PSU_ENABLE_L_PINS: sys_api::PinSet =
    PSU_ENABLE_L_PORT.pins(PSU_ENABLE_L_PINS);

// The PRESENT signals are conveniently all routed to a single port:
pub const PSU_PRESENT_L_PORT: sys_api::Port = sys_api::Port::J;
// The PRESENT signals are routed to the following pins on their port:
pub const PSU_PRESENT_L_PINS: [usize; PSU_COUNT] = [0, 1, 2, 3, 4, 5];
// Convenient mask for referring to all the PRESENT pins simultaneously, since
// we can do that, since they're all on one port.
pub const ALL_PSU_PRESENT_L_PINS: sys_api::PinSet =
    PSU_PRESENT_L_PORT.pins(PSU_PRESENT_L_PINS);

// The `PWR_OK` signals are conveniently all routed to a single port:
pub const PSU_PWR_OK_PORT: sys_api::Port = sys_api::Port::J;
// The `PWR_OK` signals are routed to the following pins on their port:
pub const PSU_PWR_OK_PINS: [usize; PSU_COUNT] = [6, 7, 8, 9, 10, 11];
// Convenient mask for referring to all the `PWR_OK` pins simultaneously, since
// we can do that, since they're all on one port.
pub const ALL_PSU_PWR_OK_PINS: sys_api::PinSet =
    PSU_PWR_OK_PORT.pins(PSU_PWR_OK_PINS);

// Our notification configuration system doesn't have any concept of arrays, so,
// collect its predefined masks into convenient arrays.
pub const PSU_PWR_OK_NOTIF: [u32; PSU_COUNT] = [
    notifications::PSU_PWR_OK_1_MASK,
    notifications::PSU_PWR_OK_2_MASK,
    notifications::PSU_PWR_OK_3_MASK,
    notifications::PSU_PWR_OK_4_MASK,
    notifications::PSU_PWR_OK_5_MASK,
    notifications::PSU_PWR_OK_6_MASK,
];

/// Type returned by generated pmbus rail functions
pub type SummonFn = fn(userlib::TaskId) -> (drv_i2c_api::I2cDevice, Option<u8>);

/// In order to get the PMBus devices by PSU index, we need a little lookup table.
pub const PSU_PMBUS_DEVS: [SummonFn; PSU_COUNT] = [
    i2c_config::pmbus::v54_psu0,
    i2c_config::pmbus::v54_psu1,
    i2c_config::pmbus::v54_psu2,
    i2c_config::pmbus::v54_psu3,
    i2c_config::pmbus::v54_psu4,
    i2c_config::pmbus::v54_psu5,
];
