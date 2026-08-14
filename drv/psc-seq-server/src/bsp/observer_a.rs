// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{PSU_COUNT, Present, i2c_config, notifications};
use drv_stm32xx_sys_api as sys_api;

pub use drv_i2c_devices::mwocp6x::Mwocp67 as Mwocp6x;
use drv_i2c_devices::mwocp6x::{self, Mwocp67};

pub const STATUS_LED: sys_api::PinSet = sys_api::Port::A.pin(3);

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
    notifications::PSU_PWR_OK_0_MASK,
    notifications::PSU_PWR_OK_1_MASK,
    notifications::PSU_PWR_OK_2_MASK,
    notifications::PSU_PWR_OK_3_MASK,
    notifications::PSU_PWR_OK_4_MASK,
    notifications::PSU_PWR_OK_5_MASK,
];

/// Type returned by generated pmbus rail functions
pub type SummonFn = fn(userlib::TaskId) -> (drv_i2c_api::I2cDevice, Option<u8>);

/// In order to get the PMBus devices by PSU index, we need a little lookup table.
pub const PSU_PMBUS_DEVS: [SummonFn; PSU_COUNT] = [
    i2c_config::pmbus::v50_main_psu0,
    i2c_config::pmbus::v50_main_psu1,
    i2c_config::pmbus::v50_main_psu2,
    i2c_config::pmbus::v50_main_psu3,
    i2c_config::pmbus::v50_main_psu4,
    i2c_config::pmbus::v50_main_psu5,
];

/// Checks whether each PSU is enabled, using PMBus. Note that the MWOCP67 PSUs
/// are enabled by default - when you plug them in, they immediately start
/// outputting 50V and continue doing so until you disable them via PMBus or
/// they are latched off by a fault.
///
/// Returns false if the PSU is present and a previous incarnation of this task
/// explicitly disabled it because it reported a fault.
///
/// Returns true if the PSU is either not present, not disabled, or the state is
/// unknown because PMBus communication failed.
///
/// These return values are somewhat unintuitive, but here are some
/// justifications:
///
/// - It may seem odd to describe an absent PSU as enabled, but this is
/// consistent with the PSC/mwocp68 implementation of this function, and it's
/// correct in the sense that the PSU will immediately start outputting power if
/// it's hot-inserted later. It also doesn't really matter because this
/// function's return value is ignored if the PSU is absent.
///
/// - We return true on a PMBus error because we don't want to trigger the fault
/// recovery process if PMBus is broken but the PSU is otherwise working and
/// outputting power. If it turns out the PSU isn't outputting power, the task
/// will notice that later and deal with it.
///
/// The unused arguments are needed by other models of PSU.
pub fn initialize_enable_states(
    _sys: &sys_api::Sys,
    devs: &mut [Mwocp67; PSU_COUNT],
    present: &[Present; PSU_COUNT],
    now: u64,
) -> [bool; PSU_COUNT] {
    core::array::from_fn(|i| {
        if present[i] == Present::Yes {
            super::retry_i2c_txn(now, super::PSU_SLOTS[i], || {
                devs[i].is_enabled()
            })
            .unwrap_or(true)
        } else {
            // PSU is absent
            true
        }
    })
}

/// Performs the enable/disable action that was requested by the state machine.
///
/// The unused arguments are needed by other models of PSU.
pub fn do_action(
    action: super::ActionRequired,
    _sys: &sys_api::Sys,
    psu_index: usize,
    dev: &mut Mwocp67,
    now: u64,
) -> Result<(), mwocp6x::Error> {
    match action {
        // The MWOCP67's output is already enabled by default when it's first
        // inserted, we don't need to explicitly enable it.
        super::ActionRequired::EnableOnInsertion => Ok(()),
        super::ActionRequired::DisableOnFault => {
            super::retry_i2c_txn(now, super::PSU_SLOTS[psu_index], || {
                dev.set_enabled(false)
            })
        }
        super::ActionRequired::ReEnableAfterFault => {
            super::retry_i2c_txn(now, super::PSU_SLOTS[psu_index], || {
                dev.clear_faults_and_latch()
            })?;
            super::retry_i2c_txn(now, super::PSU_SLOTS[psu_index], || {
                dev.set_enabled(true)
            })
        }
        // If it has been removed, then we obviously can't disable it over PMBus
        // and can't prevent it from being enabled by default as soon as it's
        // re-inserted.
        super::ActionRequired::DisableOnRemoval => Ok(()),
    }
}
