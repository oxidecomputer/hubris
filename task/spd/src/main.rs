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
use drv_i2c_api::*;
use drv_stm32g0_sys_api::Rcc;
use drv_stm32h7_gpio_api::*;
use drv_stm32h7_i2c::*;
use ringbuf::*;
use userlib::*;

task_slot!(RCC, rcc_driver);
task_slot!(GPIO, gpio_driver);
task_slot!(I2C, i2c_driver);

mod ltc4306;

fn configure_pins(pins: &[I2cPin]) {
    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = Gpio::from(gpio_driver);

    for pin in pins {
        gpio_driver
            .configure_alternate(
                pin.gpio_pins,
                OutputType::OpenDrain,
                Speed::High,
                Pull::None,
                pin.function,
            )
            .unwrap();
    }
}

//
// This is an excellent candidate to put into a non-DTCM memory region
//
static mut SPD_DATA: [u8; 8192] = [0; 8192];

const LTC4306_ADDRESS: u8 = 0b1001_010;
type Bank = (Controller, drv_i2c_api::PortIndex, Option<(Mux, Segment)>);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Found(usize),
    Ready(u32),
    Initiate(u8, bool),
    Rx(u8, u8),
    Tx(u8, Option<u8>),
    Present(u8, u8, usize),
    BankAbsent(u8),
    Absent(u8, u8, usize),
    ReadTop(usize),
    ReadBottom(usize),
    MemInitiate(usize),
    MemSetOffset(usize, u8),
    MuxState(ltc4306::State, ltc4306::State),
    None,
}

ringbuf!(Trace, 16, Trace::None);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

fn read_spd_data(
    banks: &[Bank],
    present: &mut [bool],
    spd_data: &mut [u8],
) -> usize {
    let i2c_task = I2C.get_task_id();
    let mut npresent = 0;

    //
    // For each bank, we're going to iterate over each device, reading all 512
    // bytes of SPD data from each.
    //
    for nbank in 0..banks.len() as u8 {
        let (controller, port, mux) = banks[nbank as usize];

        let addr = spd::Function::PageAddress(spd::Page(0))
            .to_device_code()
            .unwrap();
        let page = I2cDevice::new(i2c_task, controller, port, None, addr);

        if let Err(_) = page.write(&[0]) {
            //
            // If our operation fails, we are going to assume that there
            // are no DIMMs on this bank.
            //
            ringbuf_entry!(Trace::BankAbsent(nbank));
            continue;
        }

        for i in 0..spd::MAX_DEVICES {
            let mem = spd::Function::Memory(i).to_device_code().unwrap();
            let spd = I2cDevice::new(i2c_task, controller, port, mux, mem);
            let ndx = (nbank * spd::MAX_DEVICES) as usize + i as usize;
            let offs = ndx * spd::MAX_SIZE;

            //
            // Try reading the first byte; if this fails, we will assume
            // the device isn't present.
            //
            let first = match spd.read_reg::<u8, u8>(0) {
                Ok(val) => {
                    ringbuf_entry!(Trace::Present(nbank, i, ndx));
                    present[ndx] = true;
                    npresent += 1;
                    val
                }
                Err(_) => {
                    ringbuf_entry!(Trace::Absent(nbank, i, ndx));
                    continue;
                }
            };

            ringbuf_entry!(Trace::ReadBottom(ndx));

            //
            // We'll store that byte and then read 255 more.
            //
            spd_data[offs] = first;

            let base = offs + 1;
            let limit = base + 255;

            spd.read_into(&mut spd_data[base..limit]).unwrap();
        }

        //
        // Now flip over to the top page.
        //
        let addr = spd::Function::PageAddress(spd::Page(1))
            .to_device_code()
            .unwrap();
        let page = I2cDevice::new(i2c_task, controller, port, None, addr);

        //
        // We really don't expect this to fail, and if it does, tossing here
        // seems to be best option:  things are pretty wrong.
        //
        page.write(&[0]).unwrap();

        //
        // ...and two more reads for each (present) device.
        //
        for i in 0..spd::MAX_DEVICES {
            let ndx = (nbank as u8 * spd::MAX_DEVICES) as usize + i as usize;
            let offs = (ndx * spd::MAX_SIZE) + 256;

            if !present[ndx] {
                continue;
            }

            ringbuf_entry!(Trace::ReadTop(ndx));

            let mem = spd::Function::Memory(i).to_device_code().unwrap();
            let spd = I2cDevice::new(i2c_task, controller, port, mux, mem);

            let chunk = 128;
            let base = offs;
            let limit = base + chunk;
            spd.read_reg_into::<u8>(0, &mut spd_data[base..limit])
                .unwrap();

            let base = offs + chunk;
            let limit = base + chunk;
            spd.read_into(&mut spd_data[base..limit]).unwrap();
        }
    }

    npresent
}

#[export_name = "main"]
fn main() -> ! {
    let controller = &i2c_config::controllers()[0];
    let pins = i2c_config::pins();
    use i2c_config::ports::*;

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            // These should be whatever ports the dimmlets are plugged into
            const BANKS: [Bank; 2] = [
                (Controller::I2C4, i2c4_d(), None),
                (Controller::I2C4, i2c4_f(), Some((Mux::M1, Segment::S4))),
            ];
        } else if #[cfg(target_board = "gimletlet-2")] {
            // These should be whatever ports the dimmlets are plugged into
            const BANKS: [Bank; 2] = [
                (Controller::I2C3, i2c3_c(), None),
                (Controller::I2C4, i2c4_f(), None),
            ];
        } else if #[cfg(target_board = "gimlet-1")] {
            //
            // On Gimlet, we have two banks of up to 8 DIMMs apiece:
            //
            // - ABCD DIMMs are on the mid bus (I2C3, port H)
            // - EFGH DIMMS are on the read bus (I2C4, port F)
            //
            // It should go without saying that the ordering here is essential
            // to assure that the SPD data that we return for a DIMM corresponds
            // to the correct DIMM from the SoC's perspective.
            //
            const BANKS: [Bank; 2] = [
                (Controller::I2C3, i2c3_h(), None),
                (Controller::I2C4, i2c4_f(), None),
            ];
        } else {
            compile_error!("I2C target unsupported for this board");
        }
    }

    // Boolean indicating that the bank is present
    let mut present = [false; BANKS.len() * spd::MAX_DEVICES as usize];

    // Virtual offset, per virtual DIMM
    let mut voffs = [0u8; BANKS.len() * spd::MAX_DEVICES as usize];

    // The actual SPD data itself
    let spd_data = unsafe { &mut SPD_DATA };
    let mut ndelay = 0;

    //
    // It's conceivable that we are racing the sequencer and that DIMMs may
    // not be immediately visible; until we have a better way of synchronously
    // waiting on the sequencer, loop until we have found DIMMs.
    //
    loop {
        let ndimms = read_spd_data(&BANKS, &mut present, &mut spd_data[..]);

        ringbuf_entry!(Trace::Found(ndimms));

        if ndimms != 0 {
            break;
        }

        ndelay += 1;
        hl::sleep_for(10);
    }

    // Enable the controller
    let rcc_driver = Rcc::from(RCC.get_task_id());

    controller.enable(&rcc_driver);

    // Configure our pins
    configure_pins(&pins);

    ringbuf_entry!(Trace::Ready(ndelay));

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
                        let base = (bank * spd::MAX_DEVICES) as usize;
                        let ndx = base + device as usize;
                        ringbuf_entry!(Trace::MemInitiate(ndx));
                        present[ndx]
                    }
                    _ => false,
                }
            } else {
                false
            }
        } else {
            if addr == LTC4306_ADDRESS {
                ltc4306.set(ltc4306::State::init());
                true
            } else {
                false
            }
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
                    if (nbank as usize) < BANKS.len() {
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
                    let base = (bank * spd::MAX_DEVICES) as usize;
                    let ndx = base + device as usize;

                    let mut voffs = voffs.borrow_mut();
                    let offs = (ndx * spd::MAX_SIZE) + voffs[ndx] as usize;
                    let rbyte = spd_data[offs + page.get().offset()];

                    //
                    // It is actually our intent to overflow the add (that is,
                    // when performing a read at offset 0xff, the next read
                    // should be at offset 0x00), but Rust (rightfully) isn't
                    // so into that -- so unwrap what we're doing.
                    //
                    voffs[ndx] = if voffs[ndx] == u8::MAX {
                        0
                    } else {
                        voffs[ndx] + 1
                    };

                    Some(rbyte)
                }
                _ => None,
            }
        };

        ringbuf_entry!(Trace::Tx(addr, rval));
        rval
    };

    let ctrl = I2cControl {
        enable: |notification| {
            sys_irq_control(notification, true);
        },
        wfi: |notification| {
            let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
        },
    };

    controller.operate_as_target(&ctrl, &mut initiate, &mut rx, &mut tx);
}
