//! SPD proxy task
//!
//! This is (or will be) a I2C proxy for SPD data -- but at the moment it just
//! proxies sensor data.
//!

#![no_std]
#![no_main]

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

use drv_i2c_api::*;
use drv_i2c_api::{Controller, Port};
use drv_stm32h7_gpio_api::*;
use drv_stm32h7_i2c::*;
use drv_stm32h7_rcc_api::{Peripheral, Rcc};
use ringbuf::*;
use userlib::*;
use core::cell::Cell;
use core::cell::RefCell;

declare_task!(RCC, rcc_driver);
declare_task!(GPIO, gpio_driver);
declare_task!(I2C, i2c_driver);

fn configure_pin(pin: &I2cPin) {
    let gpio_driver = get_task_id(GPIO);
    let gpio_driver = Gpio::from(gpio_driver);

    gpio_driver
        .configure(
            pin.gpio_port,
            pin.mask,
            Mode::Alternate,
            OutputType::OpenDrain,
            Speed::High,
            Pull::None,
            pin.function,
        )
        .unwrap();
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Ready,
    Addr(u8),
    Rx(u8, u8),
    Tx(u8, Option<u8>),
    Present(u8, u8, usize),
    Absent(u8, u8, usize),
    ReadTop(usize),
    ReadBottom(usize),
    MemInitiate(usize),
    MemSetOffset(usize, u8),
    None,
}

ringbuf!(Trace, 16, Trace::None);

//
// This is an excellent candidate to put into a non-DTCM memory region
//
static mut SPD_DATA: [u8; 8192] = [0; 8192];

#[export_name = "main"]
fn main() -> ! {
    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            let controller = I2cController {
                controller: Controller::I2C2,
                peripheral: Peripheral::I2c2,
                registers: unsafe { &*device::I2C2::ptr() },
                notification: (1 << (2 - 1)),
            };

            let pin = I2cPin {
                controller: Controller::I2C2,
                port: Port::F,
                gpio_port: drv_stm32h7_gpio_api::Port::F,
                function: Alternate::AF4,
                mask: (1 << 0) | (1 << 1),
            };
        }
        else if #[cfg(target_board = "gimlet-1")] {
            // SP3 Proxy controller
            let controller = I2cController {
                controller: Controller::I2C1,
                peripheral: Peripheral::I2c1,
                registers: unsafe { &*device::I2C2::ptr() },
                notification: (1 << (1 - 1)),
            };

            // SMBUS_SPD_PROXY_SP3_TO_SP_SMCLK
            // SMBUS_SPD_PROXY_SP3_TO_SP_SMDAT
            let pin = I2cPin {
                controller: Controller::I2C1,
                port: Port::B,
                gpio_port: drv_stm32h7_gpio_api::Port::B,
                function: Alternate::AF4,
                mask: (1 << 6) | (1 << 7),
            };
        }
        else {
            cfg_if::cfg_if! {
                if #[cfg(feature = "standalone")] {
                    let controller = I2cController {
                        controller: Controller::I2C1,
                        peripheral: Peripheral::I2c1,
                        registers: unsafe { &*device::I2C1::ptr() },
                        notification: (1 << (1 - 1)),
                    };
                    let pin = I2cPin {
                        controller: Controller::I2C2,
                        port: Port::F,
                        gpio_port: drv_stm32h7_gpio_api::Port::F,
                        function: Alternate::AF4,
                        mask: (1 << 0) | (1 << 1),
                    };
                } else {
                    compile_error!("I2C target unsupported for this board");
                }
            }
        }
    }

    let i2c_task = get_task_id(I2C);

    const BANKS: [
        (Controller, drv_i2c_api::Port, Option<(Mux, Segment)>); 1] = [
         (Controller::I2C4, Port::D, None)
    ];

    // Boolean indicating that the bank is present
    let mut present = [false; BANKS.len() * spd::MAX_DEVICES as usize];

    // Virtual offset, per virtual DIMM
    let mut voffs = [0u8; BANKS.len() * spd::MAX_DEVICES as usize];

    // The actual SPD data itself
    let spd_data = unsafe { &mut SPD_DATA };

    //
    // For each bank, we're going to iterate over each device, reading all 512
    // bytes of SPD data from each.
    //
    for nbank in 0..BANKS.len() as u8 {
        let (controller, port, mux) = BANKS[nbank as usize];

        let addr = spd::Function::PageAddress(spd::Page(0)).to_device_code().unwrap();
        let page = I2cDevice::new(i2c_task, controller, port, None, addr);

        // 
        // Probably need to do something better than tossing on a failure
        // here -- but we also *really* don't expect it to fail
        //
        page.write(&[ 0 ]).unwrap();

        for i in 0..spd::MAX_DEVICES {
            let mem = spd::Function::Memory(i).to_device_code().unwrap();
            let spd = I2cDevice::new(i2c_task, controller, port, mux, mem);
            let ndx = (nbank * spd::MAX_DEVICES) as usize + i as usize;
            let offs = ndx * spd::MAX_SIZE as usize;

            //
            // Try reading the first byte; if this fails, we will assume
            // the device isn't present.
            //
            let first = match spd.read_reg::<u8, u8>(0) {
                Ok(val) => {
                    ringbuf_entry!(Trace::Present(nbank, i, ndx));
                    present[ndx] = true;
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
        let addr = spd::Function::PageAddress(spd::Page(1)).to_device_code().unwrap();
        let page = I2cDevice::new(i2c_task, controller, port, None, addr);

        page.write(&[ 0 ]).unwrap();

        //
        // ...and two more reads for each (present) device.
        //
        for i in 0..spd::MAX_DEVICES {
            let ndx = (nbank as u8 * spd::MAX_DEVICES) as usize + i as usize;
            let offs = (ndx * spd::MAX_SIZE as usize) + 256;

            if !present[ndx] {
                continue;
            }

            ringbuf_entry!(Trace::ReadTop(ndx));

            let mem = spd::Function::Memory(i).to_device_code().unwrap();
            let spd = I2cDevice::new(i2c_task, controller, port, mux, mem);

            let chunk = 128;
            let base = offs;
            let limit = base + chunk;
            spd.read_reg_into::<u8>(0, &mut spd_data[base..limit]).unwrap();

            let base = offs + chunk;
            let limit = base + chunk;
            spd.read_into(&mut spd_data[base..limit]).unwrap();
        }
    }

    // Enable the controller
    let rcc_driver = Rcc::from(get_task_id(RCC));

    controller.enable(&rcc_driver);

    // Configure our pins
    configure_pin(&pin);

    ringbuf_entry!(Trace::Ready);
    let pos = Cell::new(0u8);

    // Until we have virtual mux support, our virtual bank will always be 0  
    let vbank = Cell::new(0u8);

    let page = Cell::new(spd::Page(0));
    let v = RefCell::new(&mut voffs);

    let mut initiate = |addr: u8| {
        if let Some(func) = spd::Function::from_device_code(addr) {
            match func {
                spd::Function::PageAddress(_) => {
                    true
                }
                spd::Function::Memory(device) => {
                    let base = (vbank.get() * spd::MAX_DEVICES) as usize;
                    let ndx = base + device as usize;
                    ringbuf_entry!(Trace::MemInitiate(ndx));
                    present[ndx]
                }
                _ => false
            }
        } else {
            false
        }
    };

    let mut rx = |addr: u8, byte: u8| {
        ringbuf_entry!(Trace::Rx(addr, byte));

        match spd::Function::from_device_code(addr).unwrap() {
            spd::Function::PageAddress(p) => {
                page.set(p);
            }

            spd::Function::Memory(device) => {
                //
                // This is always an offset.
                //
                let base = (vbank.get() * spd::MAX_DEVICES) as usize;
                let ndx = base + device as usize;
                ringbuf_entry!(Trace::MemSetOffset(ndx, byte));
                v.borrow_mut()[ndx] = byte;
            }
            _ => {}
        }
    };

    let mut tx = |addr: u8| -> Option<u8> {
        let rval = match spd::Function::from_device_code(addr).unwrap() {
            spd::Function::Memory(device) => {
                let base = (vbank.get() * spd::MAX_DEVICES) as usize;
                let ndx = base + device as usize;

                let mut voffs = v.borrow_mut();
                let offs = (ndx * spd::MAX_SIZE as usize) + voffs[ndx] as usize;
                let rbyte = spd_data[offs + page.get().offset()];

                //
                // It is actually our intent to overflow the add (that is, when
                // performing a read at offset 0xff, the next read should be at
                // offset 0x00), but Rust (rightfully) isn't so into that -- so
                // unwrap what we're doing.
                //
                /*
                voffs[ndx] = if voffs[ndx] == u8::MAX {
                    0
                } else {
                    voffs[ndx] + 1
                };
                */
                voffs[ndx] += 1;

                Some(rbyte)
            }
            _ => {
                None
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
