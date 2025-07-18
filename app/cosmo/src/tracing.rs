// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//
// If you are cutting-and-pasting this code into another kernel (and that
// kernel is armv6m), it is hoped that you will cut-and-paste this compile
// error along with it and take heed of its admonition!
//
#[cfg(not(any(armv7m, armv8m)))]
compile_error!("ringbuf is unsound in the kernel on armv6m");

use ringbuf::*;

#[derive(Copy, Clone, PartialEq)]
enum Event {
    SyscallEnter(u32),
    SyscallExit,
    SecondarySyscallEnter,
    SecondarySyscallExit,
    IsrEnter,
    IsrExit,
    TimerIsrEnter,
    TimerIsrExit,
    ContextSwitch(usize),
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Event(Event),
    None,
}

ringbuf!(Trace, 128, Trace::None);

fn trace(event: Event) {
    ringbuf_entry!(Trace::Event(event));
}

fn syscall_enter(nr: u32) {
    trace(Event::SyscallEnter(nr));
}

fn syscall_exit() {
    trace(Event::SyscallExit);
}

fn secondary_syscall_enter() {
    trace(Event::SecondarySyscallEnter);
}

fn secondary_syscall_exit() {
    trace(Event::SecondarySyscallExit);
}

fn isr_enter() {
    trace(Event::IsrEnter);
}

fn isr_exit() {
    trace(Event::IsrExit);
}

fn timer_isr_enter() {
    trace(Event::TimerIsrEnter);
}

fn timer_isr_exit() {
    trace(Event::TimerIsrExit);
}

fn context_switch(addr: usize) {
    let rtc = unsafe { &*stm32h7::stm32h753::RTC::ptr() };
    rtc.bkpr[0].modify(|_, w| w.bkp().bits(addr as u32));
    trace(Event::ContextSwitch(addr));
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
    let rtc = unsafe { &*stm32h7::stm32h753::RTC::ptr() };

    // Unlock
    rtc.wpr.write(|w| w.key().bits(0xca));
    rtc.wpr.write(|w| w.key().bits(0x53));

    // don't erase!
    rtc.tampcr.modify(|_, w| {
        w.tamp3noerase()
            .set_bit()
            .tamp2noerase()
            .set_bit()
            .tamp1noerase()
            .set_bit()
    });

    // Save our last state
    let last = rtc.bkpr[0].read().bkp().bits();
    // And rotate
    rtc.bkpr[1].modify(|_, w| w.bkp().bits(last));

    &TRACING
}
