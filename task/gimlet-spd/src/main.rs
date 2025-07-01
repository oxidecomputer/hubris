// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SPD proxy task
//!
//! This task acts as a proxy for the Serial Presence Detect (SPD) found in
//! each DIMM.  This allows the SP to access dynamic information about the
//! DIMMs (specifically, thermal information), while still allowing the AMD
//! SoC to get the SPD information it needs for purposes of DIMM training.
//! (For more detail on the rationale for this, see RFD 88.) Each SPD EEPROM
//! has 512 bytes of information; this task will read all of it (and cache it)
//! for each present DIMM in the system.  This task is made slightly more
//! complicated by the fact that SPD allows at most 8 DIMMs to share a single
//! I2C bus; to allow for more than 8 DIMMs in the system, AMD defines a mux,
//! the mechanics of the enabling of which are encoded as an APCB token.  We
//! use AMD's default of an LTC4306, but only implement two segments, as the
//! limit of the proxy is 16 total DIMMs.
//!

#![no_std]
#![no_main]

use core::cell::Cell;
use core::cell::RefCell;
use drv_cpu_seq_api::{PowerState, NUM_SPD_BANKS};
use drv_stm32xx_i2c::target::Target;
use drv_stm32xx_i2c::{I2cPins, I2cTargetControl};
use drv_stm32xx_sys_api::{OutputType, Pull, Speed, Sys};
use ringbuf::{ringbuf, ringbuf_entry};
use task_jefe_api::Jefe;
use task_packrat_api::Packrat;
use userlib::{
    sys_irq_control, sys_recv_notification, task_slot, FromPrimitive,
};

task_slot!(SYS, sys);
task_slot!(PACKRAT, packrat);
task_slot!(JEFE, jefe);

mod ltc4306;

fn configure_pins(sys: &Sys, pins: &[I2cPins]) {
    for pin in pins {
        for gpio_pin in &[pin.scl, pin.sda] {
            sys.gpio_configure_alternate(
                *gpio_pin,
                OutputType::OpenDrain,
                Speed::High,
                Pull::None,
                pin.function,
            );
        }
    }
}

// Keep this in i2c address form
#[allow(clippy::unusual_byte_groupings)]
const LTC4306_ADDRESS: u8 = 0b1001_010;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Ready,
    Initiate(u8, bool),
    Rx(u8, u8),
    Tx(u8, Option<u8>),
    MemInitiate(u8),
    MemSetOffset(usize, u8),
    MuxState(ltc4306::State, ltc4306::State),
    None,
}

ringbuf!(Trace, 16, Trace::None);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[export_name = "main"]
fn main() -> ! {
    let packrat = Packrat::from(PACKRAT.get_task_id());
    let [controller, ..] = i2c_config::controllers();
    let controller = Target(controller);
    let pins = i2c_config::pins();

    // Virtual offset, per virtual DIMM
    let mut voffs = [0u8; NUM_SPD_BANKS * spd::MAX_DEVICES as usize];

    // Wait for entry to A2 before we enable our i2c controller.
    let jefe = Jefe::from(JEFE.get_task_id());
    loop {
        // This laborious list is intended to ensure that new power states
        // have to be added explicitly here.
        match PowerState::from_u32(jefe.get_state()) {
            Some(PowerState::A2)
            | Some(PowerState::A2PlusFans)
            | Some(PowerState::A0)
            | Some(PowerState::A0PlusHP)
            | Some(PowerState::A0Reset)
            | Some(PowerState::A0Thermtrip) => {
                break;
            }
            None => {
                // This happens before we're in a valid power state.
                //
                // Only listen to our Jefe notification.
                sys_recv_notification(notifications::JEFE_STATE_CHANGE_MASK);
            }
        }
    }

    // Enable the controller
    let sys = Sys::from(SYS.get_task_id());

    controller.enable(&sys);

    // Configure our pins
    configure_pins(&sys, &pins);

    ringbuf_entry!(Trace::Ready);

    //
    // Initialize our virtual state.  Note that we initialize with bank 0
    // visible.
    //
    let ltc4306 = Cell::new(ltc4306::State::init());
    let vbank = Cell::new(Some(0u8));
    let page = Cell::new(spd::Page(0));
    let voffs = RefCell::new(&mut voffs);

    //
    // For initiation, we only allow SPD-related addresses if the mux has
    // selected a valid segment.
    //
    let mut initiate = |addr: u8| {
        let rval = if let Some(func) = spd::Function::from_device_code(addr) {
            if let Some(bank) = vbank.get() {
                match func {
                    spd::Function::PageAddress(_) => true,
                    spd::Function::Memory(device) => {
                        let base = bank * spd::MAX_DEVICES;
                        let ndx = base + device;
                        ringbuf_entry!(Trace::MemInitiate(ndx));
                        packrat.get_spd_present(ndx)
                    }
                    _ => false,
                }
            } else {
                false
            }
        } else if addr == LTC4306_ADDRESS {
            ltc4306.set(ltc4306::State::init());
            true
        } else {
            false
        };

        ringbuf_entry!(Trace::Initiate(addr, rval));
        rval
    };

    let mut rx = |addr: u8, byte: u8| {
        ringbuf_entry!(Trace::Rx(addr, byte));

        if addr == LTC4306_ADDRESS {
            let state = ltc4306.get();
            let nstate = state.rx(byte, |nbank| {
                //
                // For any segment that exceeds the banks that we've been
                // configured with -- or for any illegal segment -- we'll set
                // our bank to None, which will make our bus appear empty
                // except for the mux.
                //
                if let Some(nbank) = nbank {
                    if (nbank as usize) < NUM_SPD_BANKS {
                        vbank.set(Some(nbank));
                    } else {
                        vbank.set(None);
                    }
                } else {
                    vbank.set(None);
                }
            });

            ringbuf_entry!(Trace::MuxState(state, nstate));
            ltc4306.set(nstate);
        } else {
            // If our bank were invalid, we should not be here
            let bank = vbank.get().unwrap();

            match spd::Function::from_device_code(addr).unwrap() {
                spd::Function::PageAddress(p) => {
                    page.set(p);
                }

                spd::Function::Memory(device) => {
                    //
                    // This is always an offset.
                    //
                    let base = (bank * spd::MAX_DEVICES) as usize;
                    let ndx = base + device as usize;
                    ringbuf_entry!(Trace::MemSetOffset(ndx, byte));
                    voffs.borrow_mut()[ndx] = byte;
                }
                _ => {}
            }
        }
    };

    let mut tx = |addr: u8| -> Option<u8> {
        let rval = if addr == LTC4306_ADDRESS {
            let state = ltc4306.get();
            let (rval, nstate) = state.tx();
            ringbuf_entry!(Trace::MuxState(state, nstate));
            ltc4306.set(nstate);
            rval
        } else {
            // As with rx, if our bank were invalid, we should not be here
            let bank = vbank.get().unwrap();

            match spd::Function::from_device_code(addr).unwrap() {
                spd::Function::Memory(device) => {
                    let base = bank * spd::MAX_DEVICES;
                    let ndx = base + device;

                    let mut voffs = voffs.borrow_mut();
                    let offs = voffs[ndx as usize] as usize;
                    let rbyte =
                        packrat.get_spd_data(ndx, offs + page.get().offset());

                    // It is our intent to overflow the add (that is, when
                    // performing a read at offset 0xff, the next read should
                    // be at offset 0x00).
                    voffs[ndx as usize] = voffs[ndx as usize].wrapping_add(1);

                    Some(rbyte)
                }
                _ => None,
            }
        };

        ringbuf_entry!(Trace::Tx(addr, rval));
        rval
    };

    let ctrl = I2cTargetControl {
        enable: |notification| {
            sys_irq_control(notification, true);
        },
        wfi: |notification| {
            sys_recv_notification(notification);
        },
    };

    controller.operate_as_target(&ctrl, &mut initiate, &mut rx, &mut tx);
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
