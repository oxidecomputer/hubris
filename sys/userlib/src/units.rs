// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//!
//! Tuple structs for units that are useful in the real world
//!

/// Degrees Celsius
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Celsius(pub f32);

/// Rotations per minute
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Rpm(pub u16);

/// Volts of potential
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Volts(pub f32);

/// Amperes of current
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Amperes(pub f32);

/// Ohms of resistence
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Ohms(pub f32);
