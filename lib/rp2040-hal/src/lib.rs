//! A crate for interfacing with the rp2040
#![no_std]

pub use rp2040_hal_macros::entry;

/// Re-export of the `boot2` crate
pub use rp2040_boot2 as boot;
/// Re-export the `pac` crate
pub use rp2040_pac as pac;

/// Resets the given list of hw resets using the passed in peripherals
#[macro_export]
macro_rules! reset_hw {
    ($peripherals:ident, [$($reset_field:ident),+ $(,)?]) => {
        // Set and then unset the peripherals
        $peripherals.RESETS
            .reset()
            // Use modify instead of `write` so we don't reset everything
            // (which can crash the system, since some of the things are needed for
            // running the system)
            .modify(|_, writer| {
                $(
                    writer.$reset_field().set_bit();
                )*
                writer
            });
        $peripherals.RESETS.reset().modify(|_, writer| {
            $(
                writer.$reset_field().clear_bit();
            )*
            writer
        });
        while {
            let regs = $peripherals.RESETS.reset_done().read();
            $(
                regs.$reset_field().bit_is_clear()
            )&&*
        } {}
    };
}

#[cfg(test)]
mod tests {}
