// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32xx RCC and GPIO blocks, combined for compactness.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]

cfg_if::cfg_if! {
    if #[cfg(feature = "family-stm32g0")] {
        use stm32g0 as pac;

        #[cfg(feature = "g030")]
        use stm32g0::stm32g030 as device;

        #[cfg(feature = "g031")]
        use stm32g0::stm32g031 as device;

        #[cfg(feature = "g070")]
        use stm32g0::stm32g070 as device;

        #[cfg(feature = "g0b1")]
        use stm32g0::stm32g0b1 as device;
    } else if #[cfg(feature = "family-stm32h7")] {
        use stm32h7 as pac;

        #[cfg(feature = "h743")]
        use stm32h7::stm32h743 as device;
        #[cfg(feature = "h753")]
        use stm32h7::stm32h753 as device;
    } else {
        compile_error!("unsupported SoC family");
    }
}

use drv_stm32xx_gpio_common::{server::get_gpio_regs, Port};
use drv_stm32xx_sys_api::{Group, RccError};
use idol_runtime::{NotificationHandler, RequestError};
use task_jefe_api::{Jefe, ResetReason};
use userlib::*;

task_slot!(JEFE, jefe);

trait FlagsRegister {
    /// Sets bit `index` in the register, preserving other bits.
    ///
    /// # Safety
    ///
    /// This is unsafe because, in theory, you might be able to find a register
    /// where setting a bit can imperil memory safety. It is your responsibility
    /// not to use this on such registers.
    unsafe fn set_bit(&self, index: u8);

    /// Clears bit `index` in the register, preserving other bits.
    ///
    /// # Safety
    ///
    /// This is unsafe because, in theory, you might be able to find a register
    /// where clearing a bit can imperil memory safety. It is your
    /// responsibility not to use this on such registers.
    unsafe fn clear_bit(&self, index: u8);
}

impl<S> FlagsRegister for pac::Reg<S>
where
    S: pac::RegisterSpec<Ux = u32> + pac::Readable + pac::Writable,
{
    unsafe fn set_bit(&self, index: u8) {
        self.modify(|r, w| unsafe { w.bits(r.bits() | 1 << index) });
    }

    unsafe fn clear_bit(&self, index: u8) {
        self.modify(|r, w| unsafe { w.bits(r.bits() & !(1 << index)) });
    }
}

#[export_name = "main"]
fn main() -> ! {
    // From thin air, pluck a pointer to the RCC register block.
    //
    // Safety: this is needlessly unsafe in the API. The RCC is essentially a
    // static, and we access it through a & reference so aliasing is not a
    // concern. Were it literally a static, we could just reference it.
    let rcc = unsafe { &*device::RCC::ptr() };

    // Global setup.
    cfg_if::cfg_if! {
        if #[cfg(feature = "family-stm32g0")] {
            rcc.iopenr.write(|w| {
                w.iopaen()
                    .set_bit()
                    .iopben()
                    .set_bit()
                    .iopcen()
                    .set_bit()
                    .iopden()
                    .set_bit()
                    .iopfen()
                    .set_bit();
                #[cfg(feature = "g0b1")]
                w.iopeen().set_bit();
                w
            });
        } else if #[cfg(feature = "family-stm32h7")] {
            rcc.ahb4enr.write(|w| {
                w.gpioaen()
                    .set_bit()
                    .gpioben()
                    .set_bit()
                    .gpiocen()
                    .set_bit()
                    .gpioden()
                    .set_bit()
                    .gpioeen()
                    .set_bit()
                    .gpiofen()
                    .set_bit()
                    .gpiogen()
                    .set_bit()
                    .gpiohen()
                    .set_bit()
                    .gpioien()
                    .set_bit()
                    .gpiojen()
                    .set_bit()
                    .gpioken()
                    .set_bit()
            });
        }
    }

    // Read RCC_RSR and inform Jefe why we reset.
    if let Some(reason) = try_read_reset_reason(rcc) {
        Jefe::from(JEFE.get_task_id()).set_reset_reason(reason);
    }

    // Field messages.
    let mut buffer = [0u8; idl::INCOMING_SIZE];
    let mut server = ServerImpl { rcc };
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl<'a> {
    rcc: &'a device::rcc::RegisterBlock,
}

impl ServerImpl<'_> {
    fn unpack_raw(raw: u32) -> Result<(Group, u8), RequestError<RccError>> {
        let bit: u8 = (raw & 0x1F) as u8;
        let bus =
            Group::from_u32(raw >> 5).ok_or(RccError::NoSuchPeripheral)?;
        // TODO: this lets people refer to bit indices that are not included in
        // the Peripheral enum, which is not great. Fixing this by deriving
        // FromPrimitive for Peripheral results in _really expensive_ checking
        // code. We could do better.
        Ok((bus, bit))
    }
}

impl idl::InOrderSysImpl for ServerImpl<'_> {
    fn enable_clock_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        let (group, bit) = Self::unpack_raw(raw)?;
        enable_clock(self.rcc, group, bit);
        Ok(())
    }

    fn disable_clock_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        let (group, bit) = Self::unpack_raw(raw)?;
        disable_clock(self.rcc, group, bit);
        Ok(())
    }

    fn enter_reset_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        let (group, bit) = Self::unpack_raw(raw)?;
        enter_reset(self.rcc, group, bit);
        Ok(())
    }

    fn leave_reset_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        let (group, bit) = Self::unpack_raw(raw)?;
        leave_reset(self.rcc, group, bit);
        Ok(())
    }

    fn gpio_configure_raw(
        &mut self,
        _: &RecvMessage,
        port: Port,
        pins: u16,
        packed_attributes: u16,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        unsafe { get_gpio_regs(port) }.configure(pins, packed_attributes);
        Ok(())
    }

    fn gpio_set_reset(
        &mut self,
        _: &RecvMessage,
        port: Port,
        set_pins: u16,
        reset_pins: u16,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        unsafe { get_gpio_regs(port) }.set_reset(set_pins, reset_pins);
        Ok(())
    }

    fn gpio_toggle(
        &mut self,
        _: &RecvMessage,
        port: Port,
        pins: u16,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        unsafe { get_gpio_regs(port) }.toggle(pins);
        Ok(())
    }

    fn gpio_read_input(
        &mut self,
        _: &RecvMessage,
        port: Port,
    ) -> Result<u16, RequestError<core::convert::Infallible>> {
        Ok(unsafe { get_gpio_regs(port) }.read())
    }

    fn read_uid(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u32; 3], RequestError<core::convert::Infallible>> {
        Ok(drv_stm32xx_uid::read_uid())
    }
}

impl NotificationHandler for ServerImpl<'_> {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "family-stm32g0")] {
        fn enable_clock(
            rcc: &device::rcc::RegisterBlock,
            group: Group,
            bit: u8,
        ) {
            match group {
                Group::Iop => unsafe { rcc.iopenr.set_bit(bit) },
                Group::Ahb => unsafe { rcc.ahbenr.set_bit(bit) },
                Group::Apb1 => unsafe { rcc.apbenr1.set_bit(bit) },
                Group::Apb2 => unsafe { rcc.apbenr2.set_bit(bit) },
            }
        }

        fn disable_clock(
            rcc: &device::rcc::RegisterBlock,
            group: Group,
            bit: u8,
        ) {
            match group {
                Group::Iop => unsafe { rcc.iopenr.clear_bit(bit) },
                Group::Ahb => unsafe { rcc.ahbenr.clear_bit(bit) },
                Group::Apb1 => unsafe { rcc.apbenr1.clear_bit(bit) },
                Group::Apb2 => unsafe { rcc.apbenr2.clear_bit(bit) },
            }
        }

        fn enter_reset(
            rcc: &device::rcc::RegisterBlock,
            group: Group,
            bit: u8,
        ) {
            match group {
                Group::Iop => unsafe { rcc.ioprstr.set_bit(bit) },
                Group::Ahb => unsafe { rcc.ahbrstr.set_bit(bit) },
                Group::Apb1 => unsafe { rcc.apbrstr1.set_bit(bit) },
                Group::Apb2 => unsafe { rcc.apbrstr2.set_bit(bit) },
            }
        }

        fn leave_reset(
            rcc: &device::rcc::RegisterBlock,
            group: Group,
            bit: u8,
        ) {
            match group {
                Group::Iop => unsafe { rcc.ioprstr.clear_bit(bit) },
                Group::Ahb => unsafe { rcc.ahbrstr.clear_bit(bit) },
                Group::Apb1 => unsafe { rcc.apbrstr1.clear_bit(bit) },
                Group::Apb2 => unsafe { rcc.apbrstr2.clear_bit(bit) },
            }
        }

        fn try_read_reset_reason(
            rcc: &device::rcc::RegisterBlock,
        ) -> Option<ResetReason> {
            // TODO map to ResetReason cases
            let bits = rcc.csr.read().bits();
            Some(ResetReason::Other(bits))
        }
    } else if #[cfg(feature = "family-stm32h7")] {
        fn enable_clock(
            rcc: &device::rcc::RegisterBlock,
            group: Group,
            bit: u8,
        ) {
            match group {
                Group::Ahb1 => unsafe { rcc.ahb1enr.set_bit(bit) },
                Group::Ahb2 => unsafe { rcc.ahb2enr.set_bit(bit) },
                Group::Ahb3 => unsafe { rcc.ahb3enr.set_bit(bit) },
                Group::Ahb4 => unsafe { rcc.ahb4enr.set_bit(bit) },
                Group::Apb1L => unsafe { rcc.apb1lenr.set_bit(bit) },
                Group::Apb1H => unsafe { rcc.apb1henr.set_bit(bit) },
                Group::Apb2 => unsafe { rcc.apb2enr.set_bit(bit) },
                Group::Apb3 => unsafe { rcc.apb3enr.set_bit(bit) },
                Group::Apb4 => unsafe { rcc.apb4enr.set_bit(bit) },
            }
        }

        fn disable_clock(
            rcc: &device::rcc::RegisterBlock,
            group: Group,
            bit: u8,
        ) {
            match group {
                Group::Ahb1 => unsafe { rcc.ahb1enr.clear_bit(bit) },
                Group::Ahb2 => unsafe { rcc.ahb2enr.clear_bit(bit) },
                Group::Ahb3 => unsafe { rcc.ahb3enr.clear_bit(bit) },
                Group::Ahb4 => unsafe { rcc.ahb4enr.clear_bit(bit) },
                Group::Apb1L => unsafe { rcc.apb1lenr.clear_bit(bit) },
                Group::Apb1H => unsafe { rcc.apb1henr.clear_bit(bit) },
                Group::Apb2 => unsafe { rcc.apb2enr.clear_bit(bit) },
                Group::Apb3 => unsafe { rcc.apb3enr.clear_bit(bit) },
                Group::Apb4 => unsafe { rcc.apb4enr.clear_bit(bit) },
            }
        }

        fn enter_reset(
            rcc: &device::rcc::RegisterBlock,
            group: Group,
            bit: u8,
        ) {
            match group {
                Group::Ahb1 => unsafe { rcc.ahb1rstr.set_bit(bit) },
                Group::Ahb2 => unsafe { rcc.ahb2rstr.set_bit(bit) },
                Group::Ahb3 => unsafe { rcc.ahb3rstr.set_bit(bit) },
                Group::Ahb4 => unsafe { rcc.ahb4rstr.set_bit(bit) },
                Group::Apb1L => unsafe { rcc.apb1lrstr.set_bit(bit) },
                Group::Apb1H => unsafe { rcc.apb1hrstr.set_bit(bit) },
                Group::Apb2 => unsafe { rcc.apb2rstr.set_bit(bit) },
                Group::Apb3 => unsafe { rcc.apb3rstr.set_bit(bit) },
                Group::Apb4 => unsafe { rcc.apb4rstr.set_bit(bit) },
            }
        }

        fn leave_reset(
            rcc: &device::rcc::RegisterBlock,
            group: Group,
            bit: u8,
        ) {
            match group {
                Group::Ahb1 => unsafe { rcc.ahb1rstr.clear_bit(bit) },
                Group::Ahb2 => unsafe { rcc.ahb2rstr.clear_bit(bit) },
                Group::Ahb3 => unsafe { rcc.ahb3rstr.clear_bit(bit) },
                Group::Ahb4 => unsafe { rcc.ahb4rstr.clear_bit(bit) },
                Group::Apb1L => unsafe { rcc.apb1lrstr.clear_bit(bit) },
                Group::Apb1H => unsafe { rcc.apb1hrstr.clear_bit(bit) },
                Group::Apb2 => unsafe { rcc.apb2rstr.clear_bit(bit) },
                Group::Apb3 => unsafe { rcc.apb3rstr.clear_bit(bit) },
                Group::Apb4 => unsafe { rcc.apb4rstr.clear_bit(bit) },
            }
        }

        fn try_read_reset_reason(
            rcc: &device::rcc::RegisterBlock,
        ) -> Option<ResetReason> {
            bitflags::bitflags! {
                // See RM0433 section 8.7.39 (RCC_RSR).
                #[derive(Copy, Clone, Debug, Eq, PartialEq)]
                #[repr(transparent)]
                pub struct ResetFlags: u32 {
                    const LPWR = 1 << 30;
                    const WWDG1 = 1 << 28;
                    const IWDG1 = 1 << 26;
                    const SFT = 1 << 24;
                    const POR = 1 << 23;
                    const PIN = 1 << 22;
                    const BOR = 1 << 21;
                    const D2 = 1 << 20;
                    const D1 = 1 << 19;
                    const CPU = 1 << 17;
                }
            }

            // See RM0433 section 8.4.4 table 55, which defines the collection
            // of pins set for each of the following reset situations.
            const POWER_ON_RESET: ResetFlags = ResetFlags::from_bits_truncate(
                ResetFlags::CPU.bits()
                | ResetFlags::D1.bits()
                | ResetFlags::D2.bits()
                | ResetFlags::BOR.bits()
                | ResetFlags::PIN.bits()
                | ResetFlags::POR.bits()
            );
            const PIN_RESET: ResetFlags = ResetFlags::from_bits_truncate(
                ResetFlags::CPU.bits() | ResetFlags::PIN.bits()
            );
            const BROWNOUT_RESET: ResetFlags = ResetFlags::from_bits_truncate(
                ResetFlags::CPU.bits()
                | ResetFlags::BOR.bits()
                | ResetFlags::PIN.bits()
            );
            const SYSTEM_RESET: ResetFlags = ResetFlags::from_bits_truncate(
                ResetFlags::CPU.bits()
                | ResetFlags::PIN.bits()
                | ResetFlags::SFT.bits()
            );
            const WWDG1_RESET: ResetFlags = ResetFlags::from_bits_truncate(
                ResetFlags::CPU.bits()
                | ResetFlags::PIN.bits()
                | ResetFlags::WWDG1.bits()
            );
            const IWDG1_RESET: ResetFlags = ResetFlags::from_bits_truncate(
                ResetFlags::CPU.bits()
                | ResetFlags::PIN.bits()
                | ResetFlags::IWDG1.bits()
            );
            const LOW_POWER_SECURITY_RESET: ResetFlags =
                ResetFlags::from_bits_truncate(
                    ResetFlags::CPU.bits()
                    | ResetFlags::PIN.bits()
                    | ResetFlags::LPWR.bits()
                );

            let rsr = rcc.rsr.read();
            let bits = rsr.bits();
            if bits == 0 {
                // RSR has been cleared; maybe our task has restarted? No
                // matter how we got here, we don't know why we most
                // recently reset.
                return None;
            }

            let flags = ResetFlags::from_bits_truncate(bits);
            let reason = match flags {
                POWER_ON_RESET => ResetReason::PowerOn,
                PIN_RESET => ResetReason::Pin,
                SYSTEM_RESET => ResetReason::SystemCall,
                BROWNOUT_RESET => ResetReason::Brownout,
                WWDG1_RESET => ResetReason::SystemWatchdog,
                IWDG1_RESET => ResetReason::IndependentWatchdog,
                LOW_POWER_SECURITY_RESET => ResetReason::LowPowerSecurity,
                ResetFlags::D1 | ResetFlags::D2 => ResetReason::ExitStandby,
                _ => ResetReason::Other(bits),
            };

            // Clear RSR.
            rcc.rsr.modify(|_, w| w.rmvf().set_bit());

            Some(reason)
        }
    } else {
        compile_error!("unsupported SoC family");
    }
}

mod idl {
    use super::{Port, RccError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
