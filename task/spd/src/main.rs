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
use drv_stm32h7_i2c::*;
use drv_stm32xx_sys_api::*;
use ringbuf::*;
use userlib::*;

use idol_runtime::{ClientError, Leased, LenLimit, RequestError};

task_slot!(SYS, sys);

mod ltc4306;

fn configure_pins(pins: &[I2cPin]) {
    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    for pin in pins {
        sys.gpio_configure_alternate(
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
    Ready,
    Initiate(u8, bool),
    Rx(u8, u8),
    Tx(u8, Option<u8>),
    MemInitiate(usize),
    MemSetOffset(usize, u8),
    MuxState(ltc4306::State, ltc4306::State),

    DataUpdate {
        index: u8,
        page1: bool,
        offset: u8,
        len: u8,
    },

    None,
}

ringbuf!(Trace, 16, Trace::None);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

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
        } else if #[cfg(any(
            target_board = "gimlet-a",
            target_board = "gimlet-b",
        ))] {
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

    // Enable the controller
    let sys = Sys::from(SYS.get_task_id());

    controller.enable(&sys);

    // Configure our pins
    configure_pins(&pins);

    ringbuf_entry!(Trace::Ready);

    //
    // Initialize our virtual state.  Note that we initialize with bank 0
    // visible.
    //
    let ltc4306 = Cell::new(ltc4306::State::init());
    let vbank = Cell::new(Some(0u8));
    let page = Cell::new(spd::Page(0));
    let voffs = RefCell::new(&mut voffs);
    let spd_data = RefCell::new(spd_data);
    let present = RefCell::new(&mut present);

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
                        present.borrow()[ndx]
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
                    let rbyte = spd_data.borrow()[offs + page.get().offset()];

                    // It is our intent to overflow the add (that is, when
                    // performing a read at offset 0xff, the next read should
                    // be at offset 0x00).
                    voffs[ndx] = voffs[ndx].wrapping_add(1);

                    Some(rbyte)
                }
                _ => None,
            }
        };

        ringbuf_entry!(Trace::Tx(addr, rval));
        rval
    };

    let mut server = ServerImpl {
        notification_mask: 0,
        spd_data: &spd_data,
        present: &present,
    };

    controller.operate_as_target(&mut server, &mut initiate, &mut rx, &mut tx);
}

struct ServerImpl<'s> {
    notification_mask: u32,
    spd_data: &'s RefCell<&'s mut [u8; 8192]>,
    present: &'s RefCell<&'s mut [bool; 16]>,
}

impl I2cControl for ServerImpl<'_> {
    fn enable(&mut self, notification: u32) {
        sys_irq_control(notification, true);
    }
    fn wfi(&mut self, notification: u32) {
        // Store the mask to smuggle it through to our handler below.
        self.notification_mask = notification;

        // This will be relatively small, so, stack-allocated is fine.
        let mut buffer = [0; idl::INCOMING_SIZE];

        idol_runtime::dispatch_n(&mut buffer, self)
    }
}

impl idl::InOrderSpdImpl for ServerImpl<'_> {
    fn eeprom_update(
        &mut self,
        _msg: &RecvMessage,
        index: u8,
        page1: bool,
        offset: u8,
        data: LenLimit<Leased<idol_runtime::R, [u8]>, 256>,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        let eeprom_base = spd::MAX_SIZE * usize::from(index);
        let eeprom_offset = 256 * usize::from(page1) + usize::from(offset);

        if eeprom_offset + data.len() > spd::MAX_SIZE {
            return Err(ClientError::BadMessageContents.fail());
        }

        let addr = eeprom_base + eeprom_offset;

        let mut spd_data = self.spd_data.borrow_mut();

        if addr + data.len() > spd_data.len() {
            return Err(ClientError::BadMessageContents.fail());
        }

        ringbuf_entry!(Trace::DataUpdate {
            index,
            page1,
            offset,
            len: data.len() as u8,
        });

        self.present.borrow_mut()[usize::from(index)] = true;

        // With the checks above this should not be able to return Err, so, we
        // unwrap.
        data.read_range(0..data.len(), &mut spd_data[addr..addr + data.len()])
            .unwrap_lite();

        Ok(())
    }
}

impl idol_runtime::NotificationHandler for ServerImpl<'_> {
    fn current_notification_mask(&self) -> u32 {
        self.notification_mask
    }

    fn handle_notification(&mut self, _bits: u32) {
        // We do nothing here -- we use the notification purely to break us out
        // of receive to do more I2C things.
    }
}

// And the Idol bits
mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
