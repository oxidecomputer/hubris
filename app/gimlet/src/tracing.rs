// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::device;

fn syscall_enter(nr: u32) {
    uart_send16(0x5C, nr as u8);
}

fn syscall_exit() {
    uart_send16(0xCA, 0xFE);
}

fn secondary_syscall_enter() {
}

fn secondary_syscall_exit() {
}

fn isr_enter() {
    uart_send16(0x01, 0x40);
}

fn isr_exit() {
}

fn timer_isr_enter() {
}

fn timer_isr_exit() {
}

fn context_switch(addr: usize) {
    let addr = addr >> 4;
    
    uart_send16(0xC5, addr as u8);
}

fn uart_send(byte: u8) {
    let mut frame = (u32::from(byte) | 0x300) << 1;
    let gpio = unsafe { &*device::GPIOH::ptr() };
    for _ in 0..11 {
        gpio.bsrr.write(|w| {
            if frame & 1 == 0 {
                w.br4().set_bit();
            } else {
                w.bs4().set_bit();
            }
            w
        });
        frame >>= 1;
    }
}

fn uart_send16(a: u8, b: u8) {
    let pkt = u16::from(a) << 8 | u16::from(b);
    let mut frame = (u32::from(pkt) | 0x3_00_00) << 1;
    let gpio = unsafe { &*device::GPIOH::ptr() };
    for _ in 0..(16+2+1) {
        gpio.bsrr.write(|w| {
            if frame & 1 == 0 {
                w.br4().set_bit();
            } else {
                w.bs4().set_bit();
            }
            w
        });
        frame >>= 1;
    }
}

static TRACING: kern::profiling::EventsTable = kern::profiling::EventsTable {
    syscall_enter,
    syscall_exit,
    secondary_syscall_enter,
    secondary_syscall_exit,
    isr_enter,
    isr_exit,
    timer_isr_enter,
    timer_isr_exit,
    context_switch,
};

pub fn table() -> &'static kern::profiling::EventsTable {
    &TRACING
}
