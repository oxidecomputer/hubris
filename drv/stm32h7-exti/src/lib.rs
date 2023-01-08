// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use drv_stm32xx_gpio_common::Port;
use drv_stm32h7_exti_api::{Edge, ExtiError};
use userlib::*;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h747cm7")]
use stm32h7::stm32h747cm7 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

#[derive(Debug, Copy, Clone)]
struct Entry {
    task: TaskId,
    notification: u32,
}

const ENTRY_COUNT: usize = 16;

pub struct Exti {
    syscfg: &'static device::syscfg::RegisterBlock,
    exti: &'static device::exti::RegisterBlock,
    entries: [Option<Entry>; ENTRY_COUNT],
}

macro_rules! enable_pin {
    ($self:ident, $port:ident, $edge:ident, $exticr:ident, $exti:ident, $tr:ident, $mr:ident) => {
        {
            $self.syscfg.$exticr.modify(|_, w|
                unsafe { w.$exti().bits($port as u8) }
            );
            $self.exti.ftsr1.modify(|_, w|
                if $edge.contains(Edge::FALLING) {
                    w.$tr().set_bit()
                } else {
                    w.$tr().clear_bit()
                }
            );
            $self.exti.rtsr1.modify(|_, w|
                if $edge.contains(Edge::RISING) {
                    w.$tr().set_bit()
                } else {
                    w.$tr().clear_bit()
                }
            );
            #[cfg(any(feature = "h743", feature = "h753"))]
            {
                $self.exti.cpuimr1.modify(|_, w| w.$mr().set_bit());
                $self.exti.cpuemr1.modify(|_, w| w.$mr().clear_bit());
            }
            #[cfg(feature = "h747cm7")]
            {
                $self.exti.c1imr1.modify(|_, w| w.$mr().set_bit());
                $self.exti.c1emr1.modify(|_, w| w.$mr().clear_bit());
            }
        }
    };
}

macro_rules! disable_pin {
    ($self:ident, $mr:ident) => {
        {
            #[cfg(any(feature = "h743", feature = "h753"))]
            $self.exti.cpuimr1.modify(|_, w| w.$mr().clear_bit());
            #[cfg(feature = "h747cm7")]
            $self.exti.c1imr1.modify(|_, w| w.$mr().clear_bit());
        }
    }
}

impl Exti {

    pub fn new() -> Self {
        Self {
            syscfg: unsafe { &*device::SYSCFG::ptr() },
            exti: unsafe { &*device::EXTI::ptr() },
            entries: [None; ENTRY_COUNT],
        }
    }

    pub fn notify(&self, index: usize) {
        if let Some(entry) = self.entries[index] {
            sys_post(sys_refresh_task_id(entry.task), entry.notification);
        }
    }

    pub fn enable_gpio(
        &mut self,
        port: Port,
        index: usize,
        edges: Edge,
        task: TaskId,
        notification: u32,
    ) -> Result<(), ExtiError> {
        if index >= self.entries.len() {
            return Err(ExtiError::InvalidIndex)
        }

        if let Some(existing) = self.entries[index] {
            if existing.task.index() != task.index() || existing.notification != notification {
                return Err(ExtiError::AlreadyRegistered)
            }
        }

        self.entries[index] = Some(Entry { task, notification });

        match index {
            0 => enable_pin!(self, port, edges, exticr1, exti0, tr0, mr0),
            1 => enable_pin!(self, port, edges, exticr1, exti1, tr1, mr1),
            2 => enable_pin!(self, port, edges, exticr1, exti2, tr2, mr2),
            3 => enable_pin!(self, port, edges, exticr1, exti3, tr3, mr3),
            4 => enable_pin!(self, port, edges, exticr2, exti4, tr4, mr4),
            5 => enable_pin!(self, port, edges, exticr2, exti5, tr5, mr5),
            6 => enable_pin!(self, port, edges, exticr2, exti6, tr6, mr6),
            7 => enable_pin!(self, port, edges, exticr2, exti7, tr7, mr7),
            8 => enable_pin!(self, port, edges, exticr3, exti8, tr8, mr8),
            9 => enable_pin!(self, port, edges, exticr3, exti9, tr9, mr9),
            10 => enable_pin!(self, port, edges, exticr3, exti10, tr10, mr10),
            11 => enable_pin!(self, port, edges, exticr3, exti11, tr11, mr11),
            12 => enable_pin!(self, port, edges, exticr4, exti12, tr12, mr12),
            13 => enable_pin!(self, port, edges, exticr4, exti13, tr13, mr13),
            14 => enable_pin!(self, port, edges, exticr4, exti14, tr14, mr14),
            15 => enable_pin!(self, port, edges, exticr4, exti15, tr15, mr15),
            _ => panic!(),
        }

        Ok(())
    }

    pub fn disable_gpio(&mut self, index: usize, task: TaskId) -> Result<(), ExtiError> {
        if index >= self.entries.len() {
            return Err(ExtiError::InvalidIndex)
        }

        if let Some(existing) = self.entries[index] {
            if existing.task.index() != task.index() {
                return Err(ExtiError::NotOwner)
            }
        } else {
            return Err(ExtiError::NotRegistered)
        }

        match index {
            0 => disable_pin!(self, mr0),
            1 => disable_pin!(self, mr1),
            2 => disable_pin!(self, mr2),
            3 => disable_pin!(self, mr3),
            4 => disable_pin!(self, mr4),
            5 => disable_pin!(self, mr5),
            6 => disable_pin!(self, mr6),
            7 => disable_pin!(self, mr7),
            8 => disable_pin!(self, mr8),
            9 => disable_pin!(self, mr9),
            10 => disable_pin!(self, mr10),
            11 => disable_pin!(self, mr11),
            12 => disable_pin!(self, mr12),
            13 => disable_pin!(self, mr13),
            14 => disable_pin!(self, mr14),
            15 => disable_pin!(self, mr15),
            _ => panic!(),
        }

        Ok(())
    }

}
