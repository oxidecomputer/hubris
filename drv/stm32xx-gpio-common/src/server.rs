// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server support functions, types, etc.

use crate::Port;

// Every PAC includes the same traits and types describing register access. One
// would expect them to be defined in a common crate that all PACs depend upon,
// but no, they are generated whole-cloth in every PAC. As a result, we can't
// simply depend on the common types here, and we need to know which SoC family
// we're targeting to generate these SoC-family-independent APIs. :-(
cfg_if::cfg_if! {
    if #[cfg(feature = "family-stm32g0")] {
        use stm32g0 as pac;
        cfg_if::cfg_if! {
            if #[cfg(feature = "model-stm32g031")] {
                use pac::stm32g031 as device;
            } else if #[cfg(feature = "model-stm32g070")] {
                use pac::stm32g070 as device;
            } else if #[cfg(feature = "model-stm32g0b1")] {
                use pac::stm32g0b1 as device;
            } else {
                compiler_error!("unsupported or missing SoC model feature");
            }
        }
    } else if #[cfg(feature = "family-stm32h7")] {
        use stm32h7 as pac;
        cfg_if::cfg_if! {
            if #[cfg(feature = "model-stm32h743")] {
                use pac::stm32h743 as device;
            } else if #[cfg(feature = "model-stm32h753")] {
                use pac::stm32h753 as device;
            } else {
                compiler_error!("unsupported or missing SoC model feature");
            }
        }
    } else {
        compiler_error!("unsupported or missing SoC family feature");
    }
}

/// Returns a reference to the GPIO peripheral corresponding to `port`. Because
/// GPIO peripherals tend to have many different types, this hides the true type
/// behind a `&dyn AnyGpioPeriph`.
///
/// # Safety
///
/// I guess this could be used unsafely if you used it within a system using the
/// Embedded HAL which relies on uniqueness of peripheral access? In practice,
/// as long as you ensure you're not doing this, you're probably ok.
pub unsafe fn get_gpio_regs(port: Port) -> &'static dyn AnyGpioPeriph {
    match port {
        Port::A => &*device::GPIOA::ptr(),
        Port::B => &*device::GPIOB::ptr(),
        Port::C => &*device::GPIOC::ptr(),
        Port::D => &*device::GPIOD::ptr(),

        #[cfg(feature = "has-port-gpioe")]
        Port::E => &*device::GPIOE::ptr(),
        #[cfg(feature = "has-port-gpiof")]
        Port::F => &*device::GPIOF::ptr(),
        #[cfg(feature = "has-port-gpiog")]
        Port::G => &*device::GPIOG::ptr(),
        #[cfg(feature = "has-port-gpioh")]
        Port::H => &*device::GPIOH::ptr(),
        #[cfg(feature = "has-port-gpioi")]
        Port::I => &*device::GPIOI::ptr(),
        #[cfg(feature = "has-port-gpioj")]
        Port::J => &*device::GPIOJ::ptr(),
        #[cfg(feature = "has-port-gpiok")]
        Port::K => &*device::GPIOK::ptr(),
    }
}

/// A GPIO peripheral that can be manipulated in abstract ways without knowing
/// its type.
pub trait AnyGpioPeriph {
    fn configure(&self, pins: u16, atts: u16);
    fn set_reset(&self, set: u16, reset: u16);
    fn toggle(&self, pins: u16);
    fn read(&self) -> u16;
}

/// Adapter from `GpioPeriph`, the trait implemented for disjoint GPIO
/// peripheral types, to the general `AnyGpioPeriph`.
impl<T: GpioPeriph> AnyGpioPeriph for T {
    fn configure(&self, pins: u16, atts: u16) {
        // The GPIO config registers come in 1, 2, and 4-bit per field variants.
        // The user-submitted mask is already correct for the 1-bit fields; we
        // need to expand it into corresponding 2- and 4-bit masks. We use an
        // outer perfect shuffle operation for this, which interleaves zeroes
        // from the top 16 bits into the bottom 16.

        // 1 in each targeted 1bit field.
        let mask_1 = u32::from(pins);

        let lsbs_1 = mask_1;
        // 0b01 in each targeted 2bit field.
        let lsbs_2 = outer_perfect_shuffle(mask_1);
        // 0b0001 in each targeted 4bit field for low half.
        let lsbs_4l = outer_perfect_shuffle(lsbs_2 & 0xFFFF);
        // Same for high half.
        let lsbs_4h = outer_perfect_shuffle(lsbs_2 >> 16);

        // Corresponding masks, with 1s in all field bits instead of just the
        // LSB.  We use multiplication to distribute a constant over all 1
        // positions because multiplication is cheap on ARMvx-M devices.
        //
        // Overflows in these multiplications can't happen, since we're
        // basically using multiplies as a cheaper shift-and-or sequence.
        // However, the compiler doesn't always see that, and so we're using
        // explicitly wrapping operations to avoid generating overflow checks.
        // This helps to eliminate panics from the GPIO servers and (at the time
        // of this writing) produced a notable text size reduction on M0+.
        let mask_2 = lsbs_2.wrapping_mul(0b11);
        let mask_4l = lsbs_4l.wrapping_mul(0b1111);
        let mask_4h = lsbs_4h.wrapping_mul(0b1111);

        // MODER contains 16x 2-bit fields.
        let moder_val = u32::from(atts & 0b11);
        self.moder().modify(|r, w| unsafe {
            // See comment re: wrapping_mul above.
            w.bits((r.bits() & !mask_2) | moder_val.wrapping_mul(lsbs_2))
        });

        // OTYPER contains 16x 1-bit fields.
        let otyper_val = u32::from((atts >> 2) & 1);
        self.otyper().modify(|r, w| unsafe {
            // See comment re: wrapping_mul above.
            w.bits((r.bits() & !mask_1) | otyper_val.wrapping_mul(lsbs_1))
        });
        // OSPEEDR contains 16x 2-bit fields.
        let ospeedr_val = u32::from((atts >> 3) & 0b11);
        self.ospeedr().modify(|r, w| unsafe {
            // See comment re: wrapping_mul above.
            w.bits((r.bits() & !mask_2) | ospeedr_val.wrapping_mul(lsbs_2))
        });
        // PUPDR contains 16x 2-bit fields.
        let pupdr_val = u32::from((atts >> 5) & 0b11);
        self.pupdr().modify(|r, w| unsafe {
            // See comment re: wrapping_mul above.
            w.bits((r.bits() & !mask_2) | pupdr_val.wrapping_mul(lsbs_2))
        });
        // AFRx contains 8x 4-bit fields.
        let af_val = u32::from((atts >> 7) & 0b1111);
        self.afrl().modify(|r, w| unsafe {
            // See comment re: wrapping_mul above.
            w.bits((r.bits() & !mask_4l) | af_val.wrapping_mul(lsbs_4l))
        });
        self.afrh().modify(|r, w| unsafe {
            // See comment re: wrapping_mul above.
            w.bits((r.bits() & !mask_4h) | af_val.wrapping_mul(lsbs_4h))
        });
    }

    fn set_reset(&self, set: u16, reset: u16) {
        self.bsrr().write(|w| unsafe {
            w.bits((u32::from(reset) << 16) | u32::from(set))
        });
    }

    fn toggle(&self, pins: u16) {
        // Read current pin *output* states.
        let state = self.odr().read().bits() as u16;
        // Use set/reset to toggle the requested pins.
        self.set_reset(!state & pins, state & pins);
    }

    fn read(&self) -> u16 {
        self.idr().read().bits() as u16
    }
}

/// Models a GPIO peripheral on the STM32xx series.
///
/// Use this trait if you want your code to be specialized to a _specific_ GPIO
/// peripheral at compile time. If you want your code to be able to choose a
/// peripheral at runtime, as in the GPIO server, use `AnyGpioPeriph`
/// instead.
///
/// ...in practice you probably don't want to use this trait.
pub trait GpioPeriph {
    type ModeSpec: pac::RegisterSpec<Ux = u32> + pac::Readable + pac::Writable;
    type OtypeSpec: pac::RegisterSpec<Ux = u32> + pac::Readable + pac::Writable;
    type OspeedSpec: pac::RegisterSpec<Ux = u32> + pac::Readable + pac::Writable;
    type PupdSpec: pac::RegisterSpec<Ux = u32> + pac::Readable + pac::Writable;
    type AflSpec: pac::RegisterSpec<Ux = u32> + pac::Readable + pac::Writable;
    type AfhSpec: pac::RegisterSpec<Ux = u32> + pac::Readable + pac::Writable;
    type BsrSpec: pac::RegisterSpec<Ux = u32> + pac::Writable + pac::Resettable;
    type OdSpec: pac::RegisterSpec<Ux = u32> + pac::Readable + pac::Writable;
    type IdSpec: pac::RegisterSpec<Ux = u32> + pac::Readable;

    fn moder(&self) -> &pac::Reg<Self::ModeSpec>;
    fn otyper(&self) -> &pac::Reg<Self::OtypeSpec>;
    fn ospeedr(&self) -> &pac::Reg<Self::OspeedSpec>;
    fn pupdr(&self) -> &pac::Reg<Self::PupdSpec>;
    fn afrl(&self) -> &pac::Reg<Self::AflSpec>;
    fn afrh(&self) -> &pac::Reg<Self::AfhSpec>;
    fn bsrr(&self) -> &pac::Reg<Self::BsrSpec>;
    fn odr(&self) -> &pac::Reg<Self::OdSpec>;
    fn idr(&self) -> &pac::Reg<Self::IdSpec>;
}

// We need to implement GpioPeriph for the various disjoint GPIO types. To avoid
// excessive repetetition, a macro:
macro_rules! impl_gpio_periph {
    ($module:ident) => {
        impl GpioPeriph for device::$module::RegisterBlock {
            type ModeSpec = device::$module::moder::MODER_SPEC;
            type OtypeSpec = device::$module::otyper::OTYPER_SPEC;
            type OspeedSpec = device::$module::ospeedr::OSPEEDR_SPEC;
            type PupdSpec = device::$module::pupdr::PUPDR_SPEC;
            type AflSpec = device::$module::afrl::AFRL_SPEC;
            type AfhSpec = device::$module::afrh::AFRH_SPEC;
            type BsrSpec = device::$module::bsrr::BSRR_SPEC;
            type OdSpec = device::$module::odr::ODR_SPEC;
            type IdSpec = device::$module::idr::IDR_SPEC;

            fn moder(&self) -> &pac::Reg<Self::ModeSpec> {
                &self.moder
            }
            fn otyper(&self) -> &pac::Reg<Self::OtypeSpec> {
                &self.otyper
            }
            fn ospeedr(&self) -> &pac::Reg<Self::OspeedSpec> {
                &self.ospeedr
            }
            fn pupdr(&self) -> &pac::Reg<Self::PupdSpec> {
                &self.pupdr
            }
            fn afrl(&self) -> &pac::Reg<Self::AflSpec> {
                &self.afrl
            }
            fn afrh(&self) -> &pac::Reg<Self::AfhSpec> {
                &self.afrh
            }
            fn bsrr(&self) -> &pac::Reg<Self::BsrSpec> {
                &self.bsrr
            }
            fn odr(&self) -> &pac::Reg<Self::OdSpec> {
                &self.odr
            }
            fn idr(&self) -> &pac::Reg<Self::IdSpec> {
                &self.idr
            }
        }
    };
}

// At least G0, F4, H7, and L4 distinguish gpioa from other ports.
#[cfg(feature = "has-gpioa-type")]
impl_gpio_periph!(gpioa);

// At least G0, F4, H7, and L4 distinguish gpiob from other ports.
#[cfg(feature = "has-gpiob-type")]
impl_gpio_periph!(gpiob);

// Add add'l types here as PAC crates invent more - L4 in particular
// distinguishes gpioc, so if we support that family, gpioc would go here.

/// Interleaves bits in `input` as follows:
///
/// - Output bit 0 = input bit 0
/// - Output bit 1 = input bit 15
/// - Output bit 2 = input bit 1
/// - Output bit 3 = input bit 16
/// ...and so forth.
///
/// This is a great example of one of those bit twiddling tricks you never
/// expected to need. Method from Hacker's Delight.
const fn outer_perfect_shuffle(mut input: u32) -> u32 {
    let mut tmp = (input ^ (input >> 8)) & 0x0000ff00;
    input ^= tmp ^ (tmp << 8);
    tmp = (input ^ (input >> 4)) & 0x00f000f0;
    input ^= tmp ^ (tmp << 4);
    tmp = (input ^ (input >> 2)) & 0x0c0c0c0c;
    input ^= tmp ^ (tmp << 2);
    tmp = (input ^ (input >> 1)) & 0x22222222;
    input ^= tmp ^ (tmp << 1);
    input
}
