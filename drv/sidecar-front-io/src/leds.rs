// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use super::transceivers::{LogicalPort, LogicalPortMask};
use drv_i2c_api::I2cDevice;
use drv_i2c_devices::pca9956b::{Error, LedErr, Pca9956B};
use drv_transceivers_api::NUM_PORTS;

pub struct Leds {
    controllers: [Pca9956B; 2],
    current: u8,
    pwm: u8,
}

/// Default LED Current
///
/// This will get written into the PCA9956B IREFALL register. The goal is to
/// make these LEDs look as close to Gimlet CEM attention LEDs as possible.
/// As of build C, Gimlet is pulling 50mA through those LEDs. From the PCA9956B
/// datasheet, the calculus is I = IREFALL/256 * (900mV/Rext) * 1/4. Rext (R47
///  & R48 on QSFP Front IO) is a 1K, so a bit of math results in a desired
/// IREF value of d222 (hDE).
///
/// This value is being temporarily set to d44 (9.9mA) until a solution for
/// https://github.com/oxidecomputer/hubris/issues/982 is agreed upon.
const DEFAULT_LED_CURRENT: u8 = 44;

/// Default LED PWM
///
/// This can be used to adjust LED duty cycle. The math here is simple, just
/// PWM/256.
const DEFAULT_LED_PWM: u8 = 255;

/// There are two LED controllers, each controlling the LEDs on either the left
/// or right of the board.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum LedController {
    Left = 0,
    Right = 1,
}

// The necessary information to control a given LED.
#[derive(Copy, Clone)]
struct LedLocation {
    controller: LedController,
    output: u8,
}

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

/// LED Map
///
/// Index 0 represents port 0, 1 to port 1, and so on. The 32 QSFP ports are
/// mapped between two PCA9956Bs, split between left and right since each can
/// only drive 24 LEDs.
///
/// The System LED is wired to the left LED driver, which I pull out into its
/// own constant since we want to be able to use our `LogicalPort` and
/// `LogicalPortMask` abstractions on an `LedMap`, and those expect a u32 as the
/// underlying type.
///
/// This is the logical -> physical mapping.
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
    #[cfg(not(target_board = "sidecar-b"))]
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
    #[cfg(not(target_board = "sidecar-b"))]
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
    #[cfg(not(target_board = "sidecar-b"))]
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
    #[cfg(not(target_board = "sidecar-b"))]
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

    /// Updates the internal state for current to `value`, then sets the IREFALL
    /// register of both controllers to `value`
    pub fn set_current(&mut self, value: u8) -> Result<(), Error> {
        self.current = value;
        self.update_current(self.current)
    }

    fn update_current(&self, value: u8) -> Result<(), Error> {
        for controller in &self.controllers {
            controller.set_iref_all(value)?;
        }

        Ok(())
    }

    /// Updates the internal state for pwm to `value`. This will get pushed out
    /// to the controllers by the `update_led_state` function as needed.
    pub fn set_pwm(&mut self, value: u8) {
        self.pwm = value;
    }

    /// This is a helper function used by a driver to set the initial value of
    /// the IREFALL register when the controllers become available
    pub fn initialize_current(&self) -> Result<(), Error> {
        self.update_current(self.current)
    }

    /// Adjust System LED PWM
    pub fn update_system_led_state(&self, turn_on: bool) -> Result<(), Error> {
        let value = if turn_on { self.pwm } else { 0 };
        self.controllers[SYSTEM_LED.controller as usize]
            .set_a_led_pwm(SYSTEM_LED.output, value)
    }

    /// Takes a `mask` of which ports need their LEDs turned on, which, for any
    /// bit set in the mask, sets its PWMx register
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
        let left_errs = self.controller(LedController::Left).check_errors()?;
        let right_errs =
            self.controller(LedController::Right).check_errors()?;

        let mut summary: FullErrorSummary = FullErrorSummary {
            overtemp_left: left_errs.overtemp,
            overtemp_right: right_errs.overtemp,
            ..Default::default()
        };

        for (index, led_loc) in LED_MAP.enumerate() {
            let output: usize = led_loc.output as usize;

            let err: LedErr = match led_loc.controller {
                LedController::Left => left_errs.errors[output],
                LedController::Right => right_errs.errors[output],
            };

            match err {
                LedErr::OpenCircuit => summary.open_circuit |= index,
                LedErr::ShortCircuit => summary.short_circuit |= index,
                LedErr::Invalid => summary.invalid |= index,
                LedErr::NoError => (),
            }
        }

        summary.system_led_err = match SYSTEM_LED.controller {
            LedController::Left => left_errs.errors[SYSTEM_LED.output as usize],
            LedController::Right => {
                right_errs.errors[SYSTEM_LED.output as usize]
            }
        };

        Ok(summary)
    }
}
