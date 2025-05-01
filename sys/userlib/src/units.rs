// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//!
//! Tuple structs for units that are useful in the real world
//!

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Degrees Celsius
#[derive(
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Debug,
    FromBytes,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(C)]
pub struct Celsius(pub f32);

/// Rotations per minute
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Debug)]
pub struct Rpm(pub u16);

/// PWM duty cycle (0-100)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct PWMDuty(pub u8);

impl TryFrom<u8> for PWMDuty {
    type Error = ();
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err(())
        }
    }
}

/// Volts of potential
#[derive(Copy, Clone, PartialEq, PartialOrd, Debug)]
pub struct Volts(pub f32);

/// Amperes of current
#[derive(Copy, Clone, PartialEq, PartialOrd, Debug)]
pub struct Amperes(pub f32);

/// Ohms of resistence
#[derive(Copy, Clone, PartialEq, PartialOrd, Debug)]
pub struct Ohms(pub f32);

/// Watts of power
#[derive(Copy, Clone, PartialEq, PartialOrd, Debug)]
pub struct Watts(pub f32);
