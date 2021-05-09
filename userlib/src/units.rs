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

