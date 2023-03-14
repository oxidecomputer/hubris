// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_stm32h7_exti_api::{Edge, ExtiError};
use drv_stm32h7_exti::Exti;
use drv_stm32xx_gpio_common::Port;
use idol_runtime::RequestError;

use ringbuf::*;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h747cm7")]
use stm32h7::stm32h747cm7 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use userlib::*;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Irq { bits: u32 },
    Register { port: Port, index: usize, edges: u8, notification: u32 },
    Unregister { index: usize },
    None,
}

ringbuf!(Trace, 64, Trace::None);

const IRQ0_MASK:     u32 = 0b0000_0001;
const IRQ1_MASK:     u32 = 0b0000_0010;
const IRQ2_MASK:     u32 = 0b0000_0100;
const IRQ3_MASK:     u32 = 0b0000_1000;
const IRQ4_MASK:     u32 = 0b0001_0000;
const IRQ9_5_MASK:   u32 = 0b0010_0000;
const IRQ15_10_MASK: u32 = 0b0100_0000;

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl {
        exti: Exti::new(),
        irq_mask: 0,
        notification_mask: 0,
    };
    let mut incoming = [0u8; INCOMING_SIZE];
    loop {
        idol_runtime::dispatch_n(&mut incoming, &mut server);
    }
}

struct ServerImpl {
    exti: Exti,
    irq_mask: u16,
    notification_mask: u32,
}

impl ServerImpl {

    fn index_to_mask(index: usize) -> Option<u32> {
        match index {
            0 => Some(IRQ0_MASK),
            1 => Some(IRQ1_MASK),
            2 => Some(IRQ2_MASK),
            3 => Some(IRQ3_MASK),
            4 => Some(IRQ4_MASK),
            5 | 6 | 7 | 8 | 9 => Some(IRQ9_5_MASK),
            10 | 11 | 12 | 13 | 14 | 15 => Some(IRQ15_10_MASK),
            _ => None,
        }
    }
}

impl InOrderExtiImpl for ServerImpl {

    fn enable_gpio_raw(
        &mut self,
        msg: &RecvMessage,
        port: Port,
        index: usize,
        edges: u8,
        notification: u32,
    ) -> Result<(), RequestError<ExtiError>> {
        ringbuf_entry!(Trace::Register { port, index, edges, notification });
        
        let mask = Self::index_to_mask(index)
            .ok_or(ExtiError::InvalidIndex)?;

        self.exti.enable_gpio(
            port, index, 
            Edge::from_bits_truncate(edges), 
            msg.sender, notification
        )?;
        
        self.irq_mask |= 1 << index;

        sys_irq_control(mask, true);
        self.notification_mask |= mask;

        Ok(())
    }

    fn disable_gpio(
        &mut self,
        msg: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<ExtiError>> {
        ringbuf_entry!(Trace::Unregister { index });

        let mask = Self::index_to_mask(index)
            .ok_or(ExtiError::InvalidIndex)?;
        
        self.exti.disable_gpio(index, msg.sender)?;

        self.irq_mask &= !(1 << index);

        match mask {
            IRQ9_5_MASK => if self.irq_mask & 0b0000_0011_1110_0000 == 0 {
                sys_irq_control(mask, false);
                self.notification_mask &= !mask;
            },
            IRQ15_10_MASK => if self.irq_mask & 0b1111_1100_0000_0000 == 0 {
                sys_irq_control(mask, false);
                self.notification_mask &= !mask;
            },
            _ => {
                sys_irq_control(mask, false);
                self.notification_mask &= !mask;
            }
        }
        Ok(())
    }

}

macro_rules! handle_irq {
    ($exti:expr, $pr1:ident, $pr:ident, $idx:literal, $bits:ident, $mask:ident) => {
        if $bits & $mask != 0 {
            $pr1.modify(|_, w| w.$pr().set_bit());
            $exti.notify($idx);
            sys_irq_control($mask, true);
        }
    };
    ($exti:expr, $pr1:ident, $pr:ident, $idx:literal) => {
        if $pr1.read().$pr().bit_is_set() {
            $pr1.modify(|_, w| w.$pr().set_bit());
            $exti.notify($idx);
        }
    };
}

impl idol_runtime::NotificationHandler for ServerImpl {

    fn current_notification_mask(&self) -> u32 {
        self.notification_mask
    }

    fn handle_notification(&mut self, bits: u32) {
        ringbuf_entry!(Trace::Irq { bits });

        #[cfg(any(feature = "h743", feature = "h753"))]
        let pr1 = &unsafe { &*device::EXTI::ptr() }.cpupr1;
        #[cfg(feature = "h747cm7")]
        let pr1 = &unsafe { &*device::EXTI::ptr() }.c1pr1;

        handle_irq!(self.exti, pr1, pr0, 0, bits, IRQ0_MASK);
        handle_irq!(self.exti, pr1, pr1, 1, bits, IRQ1_MASK);
        handle_irq!(self.exti, pr1, pr2, 2, bits, IRQ2_MASK);
        handle_irq!(self.exti, pr1, pr3, 3, bits, IRQ3_MASK);
        handle_irq!(self.exti, pr1, pr4, 4, bits, IRQ4_MASK);
        if bits & IRQ9_5_MASK != 0 {
            handle_irq!(self.exti, pr1, pr5, 5);
            handle_irq!(self.exti, pr1, pr6, 6);
            handle_irq!(self.exti, pr1, pr7, 7);
            handle_irq!(self.exti, pr1, pr8, 8);
            handle_irq!(self.exti, pr1, pr9, 9);
            sys_irq_control(IRQ9_5_MASK, true);
        }
        if bits & IRQ15_10_MASK != 0 {
            handle_irq!(self.exti, pr1, pr10, 10);
            handle_irq!(self.exti, pr1, pr11, 11);
            handle_irq!(self.exti, pr1, pr12, 12);
            handle_irq!(self.exti, pr1, pr13, 13);
            handle_irq!(self.exti, pr1, pr14, 14);
            handle_irq!(self.exti, pr1, pr15, 15);
            sys_irq_control(IRQ15_10_MASK, true);
        }
    }

}

include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
