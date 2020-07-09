//! Client API for the STM32H7 GPIO server.

#![no_std]

use byteorder::LittleEndian;
use zerocopy::{AsBytes, U16};

use userlib::*;

enum Op {
    Configure = 1,
    SetReset = 2,
    ReadInput = 3,
    Toggle = 4,
}

#[derive(Copy, Clone, Debug)]
pub enum Port {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
    E = 4,
    F = 5,
    G = 6,
    H = 7,
    I = 8,
    J = 9,
    K = 10,
}

#[derive(Copy, Clone, Debug)]
pub enum Mode {
    Input = 0b00,
    Output = 0b01,
    Alternate = 0b10,
    Analog = 0b11,
}

#[derive(Copy, Clone, Debug)]
pub enum OutputType {
    PushPull = 0,
    OpenDrain = 1,
}

#[derive(Copy, Clone, Debug)]
pub enum Speed {
    Low = 0b00,
    Medium = 0b01,
    High = 0b10,
    VeryHigh = 0b11,
}

#[derive(Copy, Clone, Debug)]
pub enum Pull {
    None = 0b00,
    Up = 0b01,
    Down = 0b10,
}

#[derive(Copy, Clone, Debug)]
pub enum Alternate {
    AF0 = 0,
    AF1 = 1,
    AF2 = 2,
    AF3 = 3,
    AF4 = 4,
    AF5 = 5,
    AF6 = 6,
    AF7 = 7,
    AF8 = 8,
    AF9 = 9,
    AF10 = 10,
    AF11 = 11,
    AF12 = 12,
    AF13 = 13,
    AF14 = 14,
    AF15 = 15,
}

#[derive(Clone, Debug)]
pub struct Gpio(TaskId);

impl From<TaskId> for Gpio {
    fn from(t: TaskId) -> Self {
        Self(t)
    }
}

#[derive(Copy, Clone, Debug)]
pub enum GpioError {
    Dead = !0,
}

impl From<u32> for GpioError {
    fn from(x: u32) -> Self {
        match x {
            core::u32::MAX => GpioError::Dead,
            _ => panic!(),
        }
    }
}

impl Gpio {
    /// Configures a subset of pins in a GPIO port.
    pub fn configure(
        &self,
        port: Port,
        pins: u16,
        mode: Mode,
        output_type: OutputType,
        speed: Speed,
        pull: Pull,
        af: Alternate,
    ) -> Result<(), GpioError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct ConfigureRequest {
            port: u8,
            pins: U16<LittleEndian>,
            packed_attributes: U16<LittleEndian>,
        }

        impl hl::Call for ConfigureRequest {
            const OP: u16 = Op::Configure as u16;
            type Response = ();
            type Err = GpioError;
        }

        let packed_attributes = mode as u16
            | (output_type as u16) << 2
            | (speed as u16) << 3
            | (pull as u16) << 5
            | (af as u16) << 7;

        hl::send(
            self.0,
            &ConfigureRequest {
                port: port as u8,
                pins: U16::new(pins),
                packed_attributes: U16::new(packed_attributes),
            },
        )
    }

    /// Alters some subset of pins in a GPIO port.
    pub fn set_reset(
        &self,
        port: Port,
        set_pins: u16,
        reset_pins: u16,
    ) -> Result<(), GpioError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct SetResetRequest {
            port: u8,
            set_pins: U16<LittleEndian>,
            reset_pins: U16<LittleEndian>,
        }

        impl hl::Call for SetResetRequest {
            const OP: u16 = Op::SetReset as u16;
            type Response = ();
            type Err = GpioError;
        }

        hl::send(
            self.0,
            &SetResetRequest {
                port: port as u8,
                set_pins: U16::new(set_pins),
                reset_pins: U16::new(reset_pins),
            },
        )
    }

    /// Reads the status of the input pins on a port.
    pub fn read_input(&self, port: Port) -> Result<u16, GpioError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct ReadInputRequest(u8);

        impl hl::Call for ReadInputRequest {
            const OP: u16 = Op::ReadInput as u16;
            type Response = u16;
            type Err = GpioError;
        }

        hl::send(self.0, &ReadInputRequest(port as u8))
    }

    /// Toggles some subset of pins in a GPIO port.
    pub fn toggle(&self, port: Port, pins: u16) -> Result<(), GpioError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct ToggleRequest {
            port: u8,
            pins: U16<LittleEndian>,
        }

        impl hl::Call for ToggleRequest {
            const OP: u16 = Op::Toggle as u16;
            type Response = ();
            type Err = GpioError;
        }

        hl::send(
            self.0,
            &ToggleRequest {
                port: port as u8,
                pins: U16::new(pins),
            },
        )
    }
}
