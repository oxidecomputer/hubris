//! A crate for interfacing with the rp2040
#![no_std]

pub use rp2040_hal_macros::entry;

/// Re-export of the `boot2` crate
pub use rp2040_boot2 as boot;
/// Re-export the `pac` crate
pub use rp2040_pac as pac;

#[cfg(test)]
mod tests {}
