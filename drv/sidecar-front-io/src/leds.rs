// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use drv_i2c_api::I2cDevice;
use drv_i2c_devices::pca9956b::{Error, LedErr, Pca9956B};

pub struct Leds {
    controllers: [Pca9956B; 2],
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
    pub open_circuit: u32,
    pub short_circuit: u32,
    pub invalid: u32,
}

/// System LED IDX
///
/// Index of the System LED in the LED_MAP
const SYSTEM_LED_IDX: usize = 32;

/// LED Map
///
/// Index 0 represents port 0, 1 to port 1, and so on. Following the ports, the
/// system LED is placed at index 32 (exposed as SYSTEM_LED_IDX above).
/// The 32 QSFP ports are mapped between two PCA9956Bs, split between left and
/// right since each can only drive 24 LEDs. The System LED is wired to the left
/// LED driver.
const LED_MAP: [LedLocation; 33] = [
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
    // Port 16
    LedLocation {
        controller: LedController::Left,
        output: 3,
    },
    // Port 17
    LedLocation {
        controller: LedController::Left,
        output: 1,
    },
    // Port 18
    LedLocation {
        controller: LedController::Left,
        output: 7,
    },
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
    // System
    LedLocation {
        controller: LedController::Left,
        output: 23,
    },
];

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
        }
    }

    fn controller(&self, c: LedController) -> &Pca9956B {
        &self.controllers[c as usize]
    }

    /// Set the current to whatever DEFAULT_LED_CURRENT is
    pub fn initialize_current(&self) -> Result<(), Error> {
        self.set_current(DEFAULT_LED_CURRENT)
    }

    /// Set the current to `value`
    pub fn set_current(&self, value: u8) -> Result<(), Error> {
        for controller in &self.controllers {
            controller.set_iref_all(value)?;
        }

        Ok(())
    }

    /// Turns on the System LED to a PWM value of DEFAULT_LED_PWM
    pub fn turn_on_system_led(&self) -> Result<(), Error> {
        const SYSTEM_LED: LedLocation = LED_MAP[SYSTEM_LED_IDX];
        self.controllers[SYSTEM_LED.controller as usize]
            .set_a_led_pwm(SYSTEM_LED.output, DEFAULT_LED_PWM)
    }

    /// Takes a `mask` of which ports need their LEDs turned on, which, for any
    /// bit set in the mask, sets its PWMx register to DEFAULT_LED_PWM
    pub fn update_led_state(&self, mask: u32) -> Result<(), Error> {
        let mut data_l: [u8; 16] = [0; 16];
        let mut data_r: [u8; 16] = [0; 16];

        for (i, led_loc) in LED_MAP.iter().enumerate().take(32) {
            let bit_mask: u32 = 1 << i;
            if (mask & bit_mask) != 0 {
                let index = led_loc.output as usize;
                match led_loc.controller {
                    LedController::Left => data_l[index] = DEFAULT_LED_PWM,
                    LedController::Right => data_r[index] = DEFAULT_LED_PWM,
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

        for (i, led_loc) in LED_MAP.iter().enumerate().take(32) {
            let port_mask = 1 << i;
            let output: usize = led_loc.output as usize;

            let err: LedErr = match led_loc.controller {
                LedController::Left => left_errs.errors[output],
                LedController::Right => right_errs.errors[output],
            };

            match err {
                LedErr::OpenCircuit => summary.open_circuit |= port_mask,
                LedErr::ShortCircuit => summary.short_circuit |= port_mask,
                LedErr::Invalid => summary.invalid |= port_mask,
                LedErr::NoError => (),
            }
        }

        // handle the system LED outside the loop since it is the 33rd index
        let sys_led_loc = LED_MAP[SYSTEM_LED_IDX];
        let sys_output: usize = sys_led_loc.output as usize;
        summary.system_led_err = match sys_led_loc.controller {
            LedController::Left => left_errs.errors[sys_output],
            LedController::Right => right_errs.errors[sys_output],
        };

        Ok(summary)
    }
}
