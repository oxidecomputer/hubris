// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn triples(triples: &[u8]) {
    let gpio = unsafe {
        &*lpc55_pac::GPIO::ptr()
    };

    for &t in triples {
        let mut bits = t;

        // Raise clock.
        gpio.set[0].write(|w| unsafe { w.bits(1 << 21) });

        // Set I/Os
        for pin in [22, 25, 4] {
            if bits & 1 == 0 {
                gpio.clr[0].write(|w| unsafe { w.bits(1 << pin) });
            } else {
                gpio.set[0].write(|w| unsafe { w.bits(1 << pin) });
            }

            bits >>= 1;
        }

        // Lower clock.
        gpio.clr[0].write(|w| unsafe { w.bits(1 << 21) });
    }
}

#[inline(always)]
fn msg(kind: MsgKind, payload: u16) {
    triples(&[kind as u8, (payload >> 6) as u8, (payload >> 3) as u8, payload as u8]);
}

#[inline(always)]
fn evt(subkind: SubKind) {
    msg(MsgKind::Other, subkind as u16);
}

enum MsgKind {
    SyscallStart = 0,
    CurrentTaskChange = 1,
    Irq = 2,
    Other = 0x6,
}

enum SubKind {
    PendSvEnter = 0,
    SystickEnter = 1,

    PendSvExit = 0b001_000,
    SystickExit = 0b001_001,

    SyscallExit = 0b001_101,
    IrqExit = 0b001_111,
}

fn syscall_enter(nr: u32) {
    msg(MsgKind::SyscallStart, nr as u16);
}

fn syscall_exit() {
    evt(SubKind::SyscallExit);
}

fn secondary_syscall_enter() {
    evt(SubKind::PendSvEnter);
}

fn secondary_syscall_exit() {
    evt(SubKind::PendSvExit);
}

fn isr_enter(n: u32) {
    msg(MsgKind::Irq, n as u16);
}

fn isr_exit() {
    evt(SubKind::IrqExit);
}

fn timer_isr_enter() {
    evt(SubKind::SystickEnter);
}

fn timer_isr_exit() {
    evt(SubKind::SystickExit);
}

fn context_switch(addr: usize) {
    let offset = addr - unsafe { kern::startup::HUBRIS_TASK_TABLE_SPACE.as_ptr() as usize };
    let index = offset / core::mem::size_of::<kern::task::Task>();
    msg(MsgKind::CurrentTaskChange, index as u16);
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

pub fn setup() -> &'static kern::profiling::EventsTable {
    let gpio = unsafe {
        &*lpc55_pac::GPIO::ptr()
    };
    gpio.dirset[0].write(|w| unsafe {
        // all bits on the AUX I/O header to outputs.
        w.bits(1 << 4 | 1 << 21 | 1 << 22 | 1 << 25);
        w
    });
    &TRACING
}
