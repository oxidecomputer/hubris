// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! "Server" that brings up the FMC and lets you pooooke it.

#![no_std]
#![no_main]

use core::convert::Infallible;
use sys_api::{Alternate, OutputType, Peripheral, Port, Pull, Speed};
use userlib::*;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{NotificationHandler, RequestError};

task_slot!(SYS, sys);

fn initialize_hardware() {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    // Alright. We assume, for these purposes, that the FMC clock generation is
    // left at its reset default of using AHB3's clock. The AHB bus in general
    // is limited to a lower clock speed than the theoretical max of the FMC, so
    // if the system is _running_ it means our clock constraints are likely met.
    // We won't worry about them further.
    //
    // With the clock source reasonable, we need to turn on the clock to the
    // controller itself:
    sys.enable_clock(Peripheral::Fmc);

    // We don't need a barrier here because that call implies a kernel entry,
    // which serves as a barrier on this architecture. Programming in userland
    // is easy and fun!
    //
    // Now that the clock is on we can poke the peripheral, so, manifest it:
    let fmc = unsafe { &*device::FMC::ptr() };

    // Configure all our pins. We're configuring a subset of pins for this demo.
    // Pin mapping is as follows:
    //  B7      FMC_NL
    //
    //  D0      FMC_DA2
    //  D1      FMC_DA3
    //  D3      FMC_CLK
    //  D4      FMC_NOE
    //  D5      FMC_NWE
    //  D6      FMC_NWAIT   (also available on C6 as AF9)
    //  D7      FMC_NE1     (also available on C7 as AF9)
    //  D8      FMC_DA13
    //  D9      FMC_DA14
    //  D10     FMC_DA15
    //  D11     FMC_A16
    //  D12     FMC_A17
    //  D13     FMC_A18
    //  D14     FMC_DA0
    //  D15     FMC_DA1
    //
    //  E0      FMC_NBL0
    //  E1      FMC_NBL1
    //  E3      FMC_A19
    //  E7      FMC_DA4
    //  E8      FMC_DA5
    //  E9      FMC_DA6
    //  E10     FMC_DA7
    //  E11     FMC_DA8
    //  E12     FMC_DA9
    //  E13     FMC_DA10
    //  E14     FMC_DA11
    //  E15     FMC_DA12
    //
    //  If you're probing this on a Nucleo:
    //
    //  FMC_CLK     CN9 pin 10 (right side, lowest pin labeled as USART)
    //  FMC_NOE     CN9 pin 8 (one up from FMC_CLK)
    //  FMC_NWE     CN9 pin 6 (one up from FMC_NOE)
    //  FMC_NWAIT   CN9 pin 4 (one up from FMC_NWE)
    //  FMC_NE1     CN9 pin 2
    //
    //  FMC_DA0     CN7 pin 4
    //  FMC_DA1     CN7 pin 2
    //  FMC_DA2     CN9 pin 25
    //  FMC_DA3     CN9 pin 27
    //
    //  FMC_NBL0    CN10 pin 33
    //  FMC_NBL1    CN11, outside, fifth hole from bottom (no connector)

    let the_pins = [
        (Port::B.pin(7), Alternate::AF12),
        (
            Port::D.pins([0, 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
            Alternate::AF12,
        ),
        (
            Port::E.pins([0, 1, 3, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
            Alternate::AF12,
        ),
    ];
    for (pinset, af) in the_pins {
        sys.gpio_configure_alternate(
            pinset,
            OutputType::PushPull,
            Speed::VeryHigh,
            Pull::None,
            af,
        );
    }

    // Program up the controller bank. Don't turn it on yet.
    fmc.bcr1.write(|w| {
        // Do not set FMCEN to turn on the controller just yet.

        // BMAP default should be OK.

        // TODO should we disable the write FIFO?

        // Emit the clock continuously.
        w.cclken().set_bit();

        // Use synchronous bursts for writes.
        w.cburstrw().set_bit();
        // ...and also reads.
        w.bursten().set_bit();

        // Have FPGA, enable wait states.
        w.waiten().set_bit();

        // Set waitconfig to 1.
        w.waitcfg().set_bit();

        // Enable writes.
        w.wren().set_bit();

        // Disable NOR flash memory access (may not be necessary?)
        w.faccen().clear_bit();

        // Configure the memory as PSRAM (TODO: verify)
        unsafe {
            w.mtyp().bits(0b01);
        }

        // Turn on the memory bank.
        w.mbken().set_bit();

        // The following fields are being deliberately left in their reset
        // states:
        // - FMCEN is being left off
        // - BMAP default (no remapping) is retained
        // - Write FIFO is being left on (TODO is this correct?)
        // - CPSIZE is being left with no special behavior on page-crossing
        // - ASYNCWAIT is being left off since we're synchronous
        // - EXTMOD is being left off, since it seems to only affect async
        // - WAITCFG is being left default (TODO tweak later)
        // - WAITPOL is treating NWAIT as active low (could change if desired)
        // - MWID is being left at a 16 bit data bus.
        // - MUXEN is being left with a multiplexed A/D bus.

        w
    });

    // Bank timings!

    // Synchronous access write/read latency, minus 2. That is, 0 means 2 cycle
    // latency. Max value: 15 (for 17 cycles). NWAIT is not sampled until this
    // period has elapsed, so if you're handshaking with a device using NWAIT,
    // you almost certainly want this to be 0.
    const DATLAT: u8 = 0;
    // FMC_CLK division ratio relative to input (AHB3) clock, minus 1. Range:
    // 1..=15.
    const CLKDIV: u8 = 3; // /4, for 50 MHz (field is divisor-minus-one)

    // Bus turnaround time in FMC_CLK cycles, 0..=15
    const BUSTURN: u8 = 0;

    fmc.btr1.write(|w| {
        unsafe {
            w.datlat().bits(DATLAT);
        }
        unsafe {
            w.clkdiv().bits(CLKDIV);
        }
        unsafe {
            w.busturn().bits(BUSTURN);
        }

        // Deliberately left in reset state and/or ignored:
        // - ACCMOD: only applies when EXTMOD is set in BCR above; also probably
        //   async only
        // - DATAST: async only
        // - ADDHLD: async only
        // - ADDSET: async only
        //
        w
    });

    // BWTR1 register is irrelevant if we're not using EXTMOD, which we're not,
    // currently.

    // Turn on the controller.
    fmc.bcr1.modify(|_, w| w.fmcen().set_bit());
}

#[export_name = "main"]
fn main() -> ! {
    if cfg!(feature = "init-hw") {
        initialize_hardware();
    }

    // Safety: we're materializing our sole pointer into the FMC controller
    // space, which is fine even if it aliases (which it doesn't).
    let fmc = unsafe { &*device::FMC::ptr() };

    // Fire up a server.
    let mut server = ServerImpl { fmc };
    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    fmc: &'static device::fmc::RegisterBlock,
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

impl idl::InOrderFmcDemoImpl for ServerImpl {
    fn peek16(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
    ) -> Result<u16, RequestError<Infallible>> {
        let ptr = addr as *const u16;
        let val = unsafe { ptr.read_volatile() };
        Ok(val)
    }

    fn peek32(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
    ) -> Result<u32, RequestError<Infallible>> {
        let ptr = addr as *const u32;
        let val = unsafe { ptr.read_volatile() };
        Ok(val)
    }

    fn peek64(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
    ) -> Result<u64, RequestError<Infallible>> {
        let ptr = addr as *const u64;
        let val = unsafe { ptr.read_volatile() };
        Ok(val)
    }

    fn poke16(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
        value: u16,
    ) -> Result<(), RequestError<Infallible>> {
        let ptr = addr as *mut u16;
        unsafe { ptr.write_volatile(value) }
        Ok(())
    }

    fn poke32(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
        value: u32,
    ) -> Result<(), RequestError<Infallible>> {
        let ptr = addr as *mut u32;
        unsafe { ptr.write_volatile(value) }
        Ok(())
    }

    fn poke64(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
        value: u64,
    ) -> Result<(), RequestError<Infallible>> {
        let ptr = addr as *mut u64;
        unsafe { ptr.write_volatile(value) }
        Ok(())
    }

    fn set_burst_enable(
        &mut self,
        _msg: &RecvMessage,
        flag: bool,
    ) -> Result<(), RequestError<Infallible>> {
        self.fmc.bcr1.modify(|_, w| {
            w.bursten().bit(flag);
            w.cburstrw().bit(flag);
            w
        });
        Ok(())
    }
    fn set_write_enable(
        &mut self,
        _msg: &RecvMessage,
        flag: bool,
    ) -> Result<(), RequestError<Infallible>> {
        self.fmc.bcr1.modify(|_, w| {
            w.wren().bit(flag);
            w
        });
        Ok(())
    }
    fn set_write_fifo(
        &mut self,
        _msg: &RecvMessage,
        flag: bool,
    ) -> Result<(), RequestError<Infallible>> {
        self.fmc.bcr1.modify(|_, w| {
            // NOTE: PARAMETER IS INVERTED
            w.wfdis().bit(!flag);
            w
        });
        Ok(())
    }
    fn set_wait(
        &mut self,
        _msg: &RecvMessage,
        flag: bool,
    ) -> Result<(), RequestError<Infallible>> {
        self.fmc.bcr1.modify(|_, w| {
            w.waiten().bit(flag);
            w
        });
        Ok(())
    }
    fn set_data_latency_cycles(
        &mut self,
        _msg: &RecvMessage,
        n: u8,
    ) -> Result<(), RequestError<Infallible>> {
        let value = n.saturating_sub(2).min(15);
        self.fmc.btr1.write(|w| {
            unsafe {
                w.datlat().bits(value);
            }
            w
        });
        Ok(())
    }
    fn set_clock_divider(
        &mut self,
        _msg: &RecvMessage,
        n: u8,
    ) -> Result<(), RequestError<Infallible>> {
        let value = n.saturating_sub(1).clamp(1, 15);
        self.fmc.btr1.write(|w| {
            unsafe {
                w.clkdiv().bits(value);
            }
            w
        });
        Ok(())
    }
    fn set_bus_turnaround_cycles(
        &mut self,
        _msg: &RecvMessage,
        n: u8,
    ) -> Result<(), RequestError<Infallible>> {
        let value = n.max(15);
        self.fmc.btr1.write(|w| {
            unsafe {
                w.busturn().bits(value);
            }
            w
        });
        Ok(())
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
