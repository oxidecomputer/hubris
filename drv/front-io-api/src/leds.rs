// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use crate::transceivers::{LogicalPort, LogicalPortMask, NUM_PORTS};
use drv_i2c_api::I2cDevice;
use drv_i2c_devices::pca9956b::{Error, LedErr, Pca9956B};
use transceiver_messages::message::LedState;

/// Leds controllers and brightness state
pub struct Leds {
    /// Two PCA9956B devices, each of which control half of the LEDs
    controllers: [Pca9956B; 2],
    /// Written into the IREFALL register on the PCA9956Bs
    ///
    /// From the PCA9956B datasheet, the calculus is
    /// I = IREFALL/256 * (900mV/Rext) * 1/4. Rext (R47 & R48 on QSFP Front IO)
    /// is a 1K, so the current value is defined as: `current` * 0.225 mA.
    current: u8,
    /// Written into the PWMx registers on the PCA9956Bs
    ///
    /// The percent of time the LED is on is governed by the duty cycle
    /// calculation: `pwm`/256.
    pwm: u8,
}

/// Default written into the PCA9956B IREFALL register
///
/// The goal is to  make these LEDs look as close to Gimlet CEM attention LEDs
/// and the various System LEDs as possible. This value is being temporarily set
/// to d44 (9.9mA), this can be adjusted as required in the future.
const DEFAULT_LED_CURRENT: u8 = 44;

/// Default written into the PCA9956B PWMx registers.
const DEFAULT_LED_PWM: u8 = 255;

#[derive(Copy, Clone)]
pub struct LedStates([LedState; NUM_PORTS as usize]);

impl LedStates {
    pub fn set(mut self, mask: LogicalPortMask, state: LedState) {
        for port in mask.to_indices() {
            self.0[port.0 as usize] = state
        }
    }

    pub fn get(self, port: LogicalPort) -> LedState {
        self.0[port.0 as usize]
    }
}

impl Default for LedStates {
    fn default() -> Self {
        LedStates([LedState::Off; NUM_PORTS as usize])
    }
}

impl<'a> IntoIterator for &'a LedStates {
    type Item = &'a LedState;
    type IntoIter = core::slice::Iter<'a, LedState>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.as_slice().iter()
    }
}

/// One controller for the LEDs on the left side, one for those on the right
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum LedController {
    Left = 0,
    Right = 1,
}

// The physical location information for the control of an LED
#[derive(Copy, Clone)]
struct LedLocation {
    controller: LedController,
    output: u8,
}

/// Summary of errors related to the LED controllers
///
/// FullErrorSummary takes the errors reported by each individual PCA9956B and
/// maps them to a by-transceiver-port representation for the masks
#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub struct FullErrorSummary {
    pub overtemp_left: bool,
    pub overtemp_right: bool,
    pub system_led_err: LedErr,
    pub open_circuit: LogicalPortMask,
    pub short_circuit: LogicalPortMask,
    pub invalid: LogicalPortMask,
}

/// Logical -> physical mapping for LEDs
///
/// Index 0 represents port 0, 1 to port 1, and so on. The 32 QSFP ports are
/// mapped between two PCA9956Bs, split between left and right since each can
/// only drive 24 LEDs.
///
/// The System LED is wired to the left LED driver, which I pull out into its
/// own constant since we want to be able to use our `LogicalPort` and
/// `LogicalPortMask` abstractions on an `LedMap`, and those expect a u32 as the
/// underlying type.
struct LedMap([LedLocation; NUM_PORTS as usize]);

impl core::ops::Index<LogicalPort> for LedMap {
    type Output = LedLocation;

    fn index(&self, i: LogicalPort) -> &Self::Output {
        &self.0[i.0 as usize]
    }
}

impl<'a> IntoIterator for &'a LedMap {
    type Item = &'a LedLocation;
    type IntoIter = core::slice::Iter<'a, LedLocation>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.as_slice().iter()
    }
}

impl LedMap {
    fn enumerate(&self) -> impl Iterator<Item = (LogicalPort, &LedLocation)> {
        self.0
            .iter()
            .enumerate()
            .map(|(i, v)| (LogicalPort(i as u8), v))
    }
}

const LED_MAP: LedMap = LedMap([
    // Port 0
    LedLocation {
        controller: LedController::Left,
        output: 0,
    },
    // Port 1
    LedLocation {
        controller: LedController::Left,
        output: 2,
    },
    // Port 2
    LedLocation {
        controller: LedController::Left,
        output: 4,
    },
    // Port 3
    LedLocation {
        controller: LedController::Left,
        output: 6,
    },
    // Port 4
    LedLocation {
        controller: LedController::Left,
        output: 8,
    },
    // Port 5
    LedLocation {
        controller: LedController::Left,
        output: 10,
    },
    // Port 6
    LedLocation {
        controller: LedController::Left,
        output: 15,
    },
    // Port 7
    LedLocation {
        controller: LedController::Left,
        output: 13,
    },
    // Port 8
    LedLocation {
        controller: LedController::Right,
        output: 0,
    },
    // Port 9
    LedLocation {
        controller: LedController::Right,
        output: 2,
    },
    // Port 10
    LedLocation {
        controller: LedController::Right,
        output: 4,
    },
    // Port 11
    LedLocation {
        controller: LedController::Right,
        output: 6,
    },
    // Port 12
    LedLocation {
        controller: LedController::Right,
        output: 8,
    },
    // Port 13
    LedLocation {
        controller: LedController::Right,
        output: 10,
    },
    // Port 14
    LedLocation {
        controller: LedController::Right,
        output: 15,
    },
    // Port 15
    LedLocation {
        controller: LedController::Right,
        output: 13,
    },
    // On Rev B hardware the LED placement for port 16/17 as well as 18/19 was
    // swapped, so we correct that here.
    #[cfg(target_board = "sidecar-b")]
    // Port 16
    LedLocation {
        controller: LedController::Left,
        output: 1,
    },
    #[cfg(any(
        target_board = "sidecar-c",
        target_board = "sidecar-d",
        target_board = "medusa-a"
    ))]
    // Port 16
    LedLocation {
        controller: LedController::Left,
        output: 3,
    },
    #[cfg(target_board = "sidecar-b")]
    // Port 17
    LedLocation {
        controller: LedController::Left,
        output: 3,
    },
    #[cfg(any(
        target_board = "sidecar-c",
        target_board = "sidecar-d",
        target_board = "medusa-a"
    ))]
    // Port 17
    LedLocation {
        controller: LedController::Left,
        output: 1,
    },
    #[cfg(target_board = "sidecar-b")]
    // Port 18
    LedLocation {
        controller: LedController::Left,
        output: 5,
    },
    #[cfg(any(
        target_board = "sidecar-c",
        target_board = "sidecar-d",
        target_board = "medusa-a"
    ))]
    // Port 18
    LedLocation {
        controller: LedController::Left,
        output: 7,
    },
    #[cfg(target_board = "sidecar-b")]
    // Port 19
    LedLocation {
        controller: LedController::Left,
        output: 7,
    },
    #[cfg(any(
        target_board = "sidecar-c",
        target_board = "sidecar-d",
        target_board = "medusa-a"
    ))]
    // Port 19
    LedLocation {
        controller: LedController::Left,
        output: 5,
    },
    // Port 20
    LedLocation {
        controller: LedController::Left,
        output: 9,
    },
    // Port 21
    LedLocation {
        controller: LedController::Left,
        output: 11,
    },
    // Port 22
    LedLocation {
        controller: LedController::Left,
        output: 14,
    },
    // Port 23
    LedLocation {
        controller: LedController::Left,
        output: 12,
    },
    // Port 24
    LedLocation {
        controller: LedController::Right,
        output: 3,
    },
    // Port 25
    LedLocation {
        controller: LedController::Right,
        output: 1,
    },
    // Port 26
    LedLocation {
        controller: LedController::Right,
        output: 7,
    },
    // Port 27
    LedLocation {
        controller: LedController::Right,
        output: 5,
    },
    // Port 28
    LedLocation {
        controller: LedController::Right,
        output: 9,
    },
    // Port 29
    LedLocation {
        controller: LedController::Right,
        output: 11,
    },
    // Port 30
    LedLocation {
        controller: LedController::Right,
        output: 14,
    },
    // Port 31
    LedLocation {
        controller: LedController::Right,
        output: 12,
    },
]);

const SYSTEM_LED: LedLocation = LedLocation {
    controller: LedController::Left,
    output: 23,
};

impl Leds {
    pub fn new(
        left_controller: &I2cDevice,
        right_controller: &I2cDevice,
    ) -> Self {
        Self {
            controllers: [
                Pca9956B::new(left_controller),
                Pca9956B::new(right_controller),
            ],
            current: DEFAULT_LED_CURRENT,
            pwm: DEFAULT_LED_PWM,
        }
    }

    fn controller(&self, c: LedController) -> &Pca9956B {
        &self.controllers[c as usize]
    }

    /// Set self.current to `value`, then update the IREFALL on the controllers
    pub fn set_current(&mut self, value: u8) -> Result<(), Error> {
        self.current = value;
        self.update_current(self.current)
    }

    /// Update IREFALL to `value` on both controllers
    fn update_current(&self, value: u8) -> Result<(), Error> {
        for controller in &self.controllers {
            controller.set_iref_all(value)?;
        }

        Ok(())
    }

    /// Sets self.pwm to `value`
    ///
    /// This will get pushed out to the controllers when the `update_led_state`
    /// function is called.
    pub fn set_pwm(&mut self, value: u8) {
        self.pwm = value;
    }

    /// Helper function used by a driver to write the initial IREFALL value
    ///
    /// This should be called once the controllers become available.
    pub fn initialize_current(&self) -> Result<(), Error> {
        self.update_current(self.current)
    }

    /// Turn the System LED on or off
    pub fn update_system_led_state(&self, turn_on: bool) -> Result<(), Error> {
        let value = if turn_on { self.pwm } else { 0 };
        self.controller(SYSTEM_LED.controller)
            .set_a_led_pwm(SYSTEM_LED.output, value)
    }

    /// Turns on the LED for each bit set in `mask`
    ///
    /// For any bit set in the `mask`, sets its corresponding PWMx register
    pub fn update_led_state(&self, mask: LogicalPortMask) -> Result<(), Error> {
        let mut data_l: [u8; 16] = [0; 16];
        let mut data_r: [u8; 16] = [0; 16];

        for (index, led_loc) in LED_MAP.enumerate() {
            if mask.is_set(index) {
                let index = led_loc.output as usize;
                match led_loc.controller {
                    LedController::Left => data_l[index] = self.pwm,
                    LedController::Right => data_r[index] = self.pwm,
                }
            }
        }

        self.controller(LedController::Left)
            .set_all_led_pwm(&data_l)?;
        self.controller(LedController::Right)
            .set_all_led_pwm(&data_r)?;

        Ok(())
    }

    /// Query device registers and return a summary of observed errors
    pub fn error_summary(&self) -> Result<FullErrorSummary, Error> {
        let errs_l = self.controller(LedController::Left).check_errors()?;
        let errs_r = self.controller(LedController::Right).check_errors()?;

        let mut summary: FullErrorSummary = FullErrorSummary {
            overtemp_left: errs_l.overtemp,
            overtemp_right: errs_r.overtemp,
            ..Default::default()
        };

        for (index, led_loc) in LED_MAP.enumerate() {
            let output: usize = led_loc.output as usize;

            let err: LedErr = match led_loc.controller {
                LedController::Left => errs_l.errors[output],
                LedController::Right => errs_r.errors[output],
            };

            match err {
                LedErr::OpenCircuit => summary.open_circuit |= index,
                LedErr::ShortCircuit => summary.short_circuit |= index,
                LedErr::Invalid => summary.invalid |= index,
                LedErr::NoError => (),
            }
        }

        summary.system_led_err = match SYSTEM_LED.controller {
            LedController::Left => &errs_l,
            LedController::Right => &errs_r,
        }
        .errors[SYSTEM_LED.output as usize];

        Ok(summary)
    }
}
