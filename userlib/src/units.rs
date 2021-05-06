//!
//! Tuple structs for units that are useful in the real world
//!

/// Degrees Celsius
#[derive(Copy, Clone, Debug)]
pub struct Celsius(pub f32);

/// Rotations per minute
#[derive(Copy, Clone, Debug)]
pub struct Rpm(pub u16);

/// Volts
#[derive(Copy, Clone, Debug)]
pub struct Volts(pub f32);

/// Amperes
#[derive(Copy, Clone, Debug)]
pub struct Amperes(pub f32);

