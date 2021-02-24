//! I2C device drivers
//!
//! This crate contains (generally) all I2C device drivers, including:
//!
//! - [`adt7420`]: ADT7420 temperature sensor
//! - [`ds2482`]: DS2482-100 1-wire initiator
//! - [`max31790`]: MAX31790 fan controller

#![no_std]

pub mod adt7420;
pub mod ds2482;
pub mod max31790;
