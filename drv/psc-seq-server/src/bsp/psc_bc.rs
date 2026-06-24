// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{PSU_COUNT, Present, i2c_config, notifications};
use drv_stm32xx_sys_api as sys_api;
use sys_api::{OutputType, PinSet, Pull, Speed};

pub use drv_i2c_devices::mwocp6x::Mwocp68 as Mwocp6x;

pub const STATUS_LED: sys_api::PinSet = sys_api::Port::A.pin(3);

// The ON signals are conveniently all routed to a single port:
pub const PSU_ENABLE_L_PORT: sys_api::Port = sys_api::Port::K;

// The ON signals are routed to the following pins on their port:
const PSU_ENABLE_L_PINS: [usize; PSU_COUNT] = [0, 1, 2, 3, 4, 5];

// Convenient mask for referring to all the ON pins simultaneously, since we can
// do that, since they're all on one port.
const ALL_PSU_ENABLE_L_PINS: sys_api::PinSet =
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

/// Configures the PSU_ENABLE_L pins and returns the initial state of each PSU.
///
/// Returns false if the previous incarnation of this task explicitly disabled
/// the PSU, either because the PSU reported a fault or was hot-removed.
///
/// Returns true if this is the first generation of the task, or that the
/// previous incarnation did not disable this PSU. It does not imply that the
/// PSU is currently present!
///
/// The unused arguments are needed by other models of PSU.
pub fn initialize_enable_states(
    sys: &sys_api::Sys,
    _devs: &mut [Mwocp6x; PSU_COUNT],
    _present: &[Present; PSU_COUNT],
    _now: u64,
) -> [bool; PSU_COUNT] {
    // Check the status of the PSU ON nets, which indicate the current commanded
    // status of the PSUs. We can use this information to seed our state
    // machines, and also to make sure we don't glitch the PSUs.
    //
    // Note that, on power-on reset, these pins default to being configured
    // Analog, preventing us from reading their state. This is okay. In Analog
    // mode, an STM32 pin is defined as reading as 0, so we will see any such
    // pins as "PSU is ON" and switch the pin to input below. It is only if this
    // task has _restarted_ that we'll find pins set to input seeing 0, or
    // output seeing 1.
    let initial_psu_enabled: [bool; PSU_COUNT] = {
        let bits = sys.gpio_read(ALL_PSU_ENABLE_L_PINS);
        // ON signals are active-low, so we check for the _absence_ of the bit:
        core::array::from_fn(|i| bits & (1 << PSU_ENABLE_L_PINS[i]) == 0)
    };

    // Since we mostly just toggle the PSU ON nets between input and output, we
    // don't actually want to configure them at all at this stage. They're
    // either set input (in which case the PSU is being asked to be "on") or
    // output (in which case we're holding the PSU off, and will start a fault
    // resume sequence shortly).
    //
    // Ensure that the subset of pins that are currently undriven (which is to
    // say, ENABLE line low, PSU on) are set as inputs. Leave any pins observed
    // as 1 configured as they are. (See the rationale for this above on the
    // initial read.)
    sys.gpio_configure_input(
        {
            let mut inpins = PinSet {
                port: PSU_ENABLE_L_PORT,
                pin_mask: 0,
            };
            for (on, pinno) in
                initial_psu_enabled.into_iter().zip(PSU_ENABLE_L_PINS)
            {
                if on {
                    inpins = inpins.and_pin(pinno);
                }
            }
            // This set might be empty. That's ok; sys tolerates this.
            inpins
        },
        Pull::None,
    );

    // While we are not going to explicitly configure any pins as outputs at
    // this stage, for toggling the pins between input and output to work
    // properly, we need to pre-arrange for the pins to be high once they _are_
    // set to output. We do that here. If the pin is input, this has no effect;
    // if it's output, this should be a no-op because our previous incarnation
    // will have done this before setting it to output.
    sys.gpio_set_to(ALL_PSU_ENABLE_L_PINS, true);

    initial_psu_enabled
}

/// Performs the enable/disable action that was requested by the state machine.
///
/// The unused arguments are needed by other models of PSU.
pub fn do_action(
    action: super::ActionRequired,
    sys: &sys_api::Sys,
    psu_index: usize,
    _dev: &mut Mwocp6x,
    _now: u64,
) -> Result<(), super::mwocp6x::Error> {
    match action {
        super::ActionRequired::EnableOnInsertion
        | super::ActionRequired::ReEnableAfterFault => {
            // Enable the PSU by allowing `ENABLE_L` to float low, by no
            // longer asserting high.
            sys.gpio_configure_input(
                PSU_ENABLE_L_PORT.pin(PSU_ENABLE_L_PINS[psu_index]),
                Pull::None,
            );
        }
        super::ActionRequired::DisableOnFault
        | super::ActionRequired::DisableOnRemoval => {
            // Pull `ENABLE_L` high to disable the PSU.
            sys.gpio_configure_output(
                PSU_ENABLE_L_PORT.pin(PSU_ENABLE_L_PINS[psu_index]),
                OutputType::PushPull,
                Speed::Low,
                Pull::None,
            );
        }
    }
    Ok(())
}
