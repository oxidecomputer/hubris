// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server task for the STM32H7 SPI peripheral.
//!
//! Currently this hardcodes the clock rate.
//!
//! See the `spi-api` crate for the protocol being implemented here.
//!
//! # Why is everything `spi1`
//!
//! As noted in the `stm32h7-spi` driver, the `stm32h7` PAC has decided that all
//! SPI types should be called `spi1`.

#![no_std]
#![no_main]

use drv_spi_api::*;
use idol_runtime::{
    LeaseBufReader, LeaseBufWriter, Leased, LenLimit, RequestError, R, W,
};
use ringbuf::*;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use userlib::*;

use drv_stm32h7_spi as spi_core;
use drv_stm32xx_sys_api as sys_api;

task_slot!(SYS, sys);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Start(SpiOperation, (u16, u16)),
    Tx(u8),
    Rx(u8),
    WaitISR(u32),
    None,
}

ringbuf!(Trace, 64, Trace::None);

const IRQ_MASK: u32 = 1;

#[derive(Copy, Clone, Debug)]
struct LockState {
    task: TaskId,
    device_index: usize,
}

#[export_name = "main"]
fn main() -> ! {
    check_server_config();

    let sys = sys_api::Sys::from(SYS.get_task_id());

    let registers = unsafe { &*CONFIG.registers };

    sys.enable_clock(CONFIG.peripheral);
    sys.enter_reset(CONFIG.peripheral);
    sys.leave_reset(CONFIG.peripheral);
    let mut spi = spi_core::Spi::from(registers);

    // This should correspond to '0' in the standard SPI parlance
    spi.initialize(
        device::spi1::cfg1::MBR_A::DIV64,
        8,
        device::spi1::cfg2::COMM_A::FULLDUPLEX,
        device::spi1::cfg2::LSBFRST_A::MSBFIRST,
        device::spi1::cfg2::CPHA_A::FIRSTEDGE,
        device::spi1::cfg2::CPOL_A::IDLELOW,
        device::spi1::cfg2::SSOM_A::ASSERTED,
    );

    // Configure all devices' CS pins to be deasserted (set).
    // We leave them in GPIO output mode from this point forward.
    for device in CONFIG.devices {
        sys.gpio_set_reset(device.cs.port, device.cs.pin_mask, 0)
            .unwrap();
        sys.gpio_configure(
            device.cs.port,
            device.cs.pin_mask,
            sys_api::Mode::Output,
            sys_api::OutputType::PushPull,
            sys_api::Speed::Low,
            sys_api::Pull::None,
            sys_api::Alternate::AF1, // doesn't matter in GPIO mode
        )
        .unwrap();
    }

    // Initially, configure mux 0. This keeps us from having to deal with a "no
    // mux selected" state.
    //
    // Note that the config check routine above ensured that there _is_ a mux
    // option 0.
    //
    // We deactivate before activate to avoid pin clash if we previously crashed
    // with one of these activated.
    let current_mux_index = 0;
    for opt in &CONFIG.mux_options[1..] {
        deactivate_mux_option(&opt, &sys);
    }
    activate_mux_option(&CONFIG.mux_options[current_mux_index], &sys, &spi);

    let mut server = ServerImpl {
        spi,
        sys,
        lock_holder: None,
        current_mux_index,
    };
    let mut incoming = [0u8; INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

struct ServerImpl {
    spi: spi_core::Spi,
    sys: sys_api::Sys,
    lock_holder: Option<LockState>,
    current_mux_index: usize,
}

impl InOrderSpiImpl for ServerImpl {
    fn recv_source(&self) -> Option<userlib::TaskId> {
        self.lock_holder.map(|s| s.task)
    }

    fn closed_recv_fail(&mut self) {
        // Welp, someone had asked us to lock and then died. Release the
        // lock.
        self.lock_holder = None;
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        device_index: u8,
        dest: LenLimit<Leased<W, [u8]>, 65535>,
    ) -> Result<(), RequestError<SpiError>> {
        self.ready_writey(SpiOperation::read, device_index, None, Some(dest))
    }
    fn write(
        &mut self,
        _: &RecvMessage,
        device_index: u8,
        src: LenLimit<Leased<R, [u8]>, 65535>,
    ) -> Result<(), RequestError<SpiError>> {
        self.ready_writey(SpiOperation::write, device_index, Some(src), None)
    }
    fn exchange(
        &mut self,
        _: &RecvMessage,
        device_index: u8,
        src: LenLimit<Leased<R, [u8]>, 65535>,
        dest: LenLimit<Leased<W, [u8]>, 65535>,
    ) -> Result<(), RequestError<SpiError>> {
        self.ready_writey(
            SpiOperation::exchange,
            device_index,
            Some(src),
            Some(dest),
        )
    }
    fn lock(
        &mut self,
        rm: &RecvMessage,
        devidx: u8,
        cs_state: CsState,
    ) -> Result<(), RequestError<SpiError>> {
        let cs_asserted = cs_state == CsState::Asserted;
        let devidx = usize::from(devidx);

        // If we are locked there are more rules:
        if let Some(lockstate) = &self.lock_holder {
            // The fact that we received this message _at all_ means
            // that the sender matched our closed receive, but just
            // in case we have a server logic bug, let's check.
            assert!(lockstate.task == rm.sender);
            // The caller is not allowed to change the device index
            // once locked.
            if lockstate.device_index != devidx {
                return Err(SpiError::BadDevice.into());
            }
        }

        // OK! We are either (1) just locking now or (2) processing
        // a legal state change from the same sender.

        // Reject out-of-range devices.
        let device = CONFIG.devices.get(devidx).ok_or(SpiError::BadDevice)?;

        // If we're asserting CS, we want to *reset* the pin. If
        // we're not, we want to *set* it. Because CS is active low.
        let pin_mask = device.cs.pin_mask;
        self.sys
            .gpio_set_reset(
                device.cs.port,
                if cs_asserted { 0 } else { pin_mask },
                if cs_asserted { pin_mask } else { 0 },
            )
            .unwrap();
        self.lock_holder = Some(LockState {
            task: rm.sender,
            device_index: devidx,
        });
        Ok(())
    }

    fn release(
        &mut self,
        rm: &RecvMessage,
    ) -> Result<(), RequestError<SpiError>> {
        if let Some(lockstate) = &self.lock_holder {
            // The fact that we were able to receive this means we
            // should be locked by the sender...but double check.
            assert!(lockstate.task == rm.sender);

            let device = &CONFIG.devices[lockstate.device_index];

            // Deassert CS. If it wasn't asserted, this is a no-op.
            // If it was, this fixes that.
            self.sys
                .gpio_set_reset(device.cs.port, device.cs.pin_mask, 0)
                .unwrap();
            self.lock_holder = None;
            Ok(())
        } else {
            Err(SpiError::NothingToRelease.into())
        }
    }
}

impl ServerImpl {
    fn ready_writey(
        &mut self,
        op: SpiOperation,
        device_index: u8,
        data_src: Option<LenLimit<Leased<R, [u8]>, 65535>>,
        data_dest: Option<LenLimit<Leased<W, [u8]>, 65535>>,
    ) -> Result<(), RequestError<SpiError>> {
        let device_index = usize::from(device_index);

        // If we are locked, check that the caller isn't mistakenly
        // addressing the wrong device.
        if let Some(lockstate) = &self.lock_holder {
            if lockstate.device_index != device_index {
                return Err(SpiError::BadDevice.into());
            }
        }

        // Reject out-of-range devices.
        let device = CONFIG
            .devices
            .get(device_index)
            .ok_or(SpiError::BadDevice)?;

        // At least one lease must be provided. A failure here indicates that
        // the server stub calling this common routine is broken, not a client
        // mistake.
        if data_src.is_none() && data_dest.is_none() {
            panic!();
        }

        // Get the required transfer lengths in the src and dest directions.
        let src_len = data_src
            .as_ref()
            .map(|leased| LenLimit::len_as_u16(&leased))
            .unwrap_or(0);
        let dest_len = data_dest
            .as_ref()
            .map(|leased| LenLimit::len_as_u16(&leased))
            .unwrap_or(0);
        let overall_len = src_len.max(dest_len);

        // Zero-byte SPI transactions don't make sense and we'll
        // decline them.
        if overall_len == 0 {
            return Err(SpiError::BadTransferSize.into());
        }

        // We have a reasonable-looking request containing reasonable-looking
        // lease(s). This is our commit point.
        ringbuf_entry!(Trace::Start(op, (src_len, dest_len)));

        // Switch the mux to the requested port.
        if device.mux_index != self.current_mux_index {
            deactivate_mux_option(
                &CONFIG.mux_options[self.current_mux_index],
                &self.sys,
            );
            activate_mux_option(
                &CONFIG.mux_options[device.mux_index],
                &self.sys,
                &self.spi,
            );
            // Remember this for later to avoid unnecessary
            // switching.
            self.current_mux_index = device.mux_index;
        }

        // Make sure SPI is on.
        //
        // Due to driver limitations we will only move up to 64kiB
        // per transaction. It would be worth lifting this
        // limitation, maybe. Doing so would require managing data
        // in 64kiB chunks (because the peripheral is 16-bit) and
        // using the "reload" facility on the peripheral.
        self.spi.enable(overall_len, device.clock_divider);

        // Load transfer count and start the state machine. At this
        // point we _have_ to move the specified number of bytes
        // through (or explicitly cancel, but we don't).
        self.spi.start();

        // As you might expect, we will work from byte 0 to the end
        // of each buffer. There are two complications:
        //
        // 1. Transmit and receive can be at different positions --
        //    transmit will tend to lead receive, because the SPI
        //    unit contains FIFOs.
        //
        // 2. We're only keeping track of position in the buffers
        //    we're using: both tx and rx are `Option`.
        //
        // The BufReader/Writer types manage position tracking for us.

        // Wrap a buffer reader/writer onto whichever borrows actually exist.
        // This lets us amortize the cost of the borrow syscalls for retrieving
        // data from the caller. It doesn't appear to be useful to make this any
        // larger than the FIFO depth; for simplicity we set:
        const BUFSIZ: usize = 16;

        let mut tx: Option<LeaseBufReader<_, BUFSIZ>> =
            data_src.map(|b| LeaseBufReader::from(b.into_inner()));
        let mut rx: Option<LeaseBufWriter<_, BUFSIZ>> =
            data_dest.map(|b| LeaseBufWriter::from(b.into_inner()));

        // Enable interrupt on the conditions we're interested in.
        self.spi.enable_transfer_interrupts();

        self.spi.clear_eot();

        // We're doing this! Check if we need to control CS.
        let cs_override = self.lock_holder.is_some();
        if !cs_override {
            self.sys
                .gpio_set_reset(device.cs.port, 0, device.cs.pin_mask)
                .unwrap();
        }

        // We use this to exert backpressure on the TX state machine as the RX
        // FIFO fills. Its initial value is the configured FIFO size, because
        // the FIFO size varies on SPI blocks on the H7; it would be nice if we
        // could read the configured FIFO size out of the block, but that does
        // not appear to be possible.
        //
        // See reference manual table 409 for details.
        let mut tx_permits = FIFO_DEPTH;

        // Track number of bytes sent and received. Sent bytes will lead
        // received bytes. Received bytes indicate overall progress and
        // completion.
        let mut tx_count = 0;
        let mut rx_count = 0;

        // The end of the exchange is signaled by rx_count reaching the
        // overall_len. This is true even if the caller's rx lease is shorter or
        // missing, because we have to pull bytes from the FIFO to avoid overrun
        // conditions.
        while rx_count < overall_len {
            // At the end of this loop we're going to sleep if there's no
            // obvious work to be done. Sleeping is not free, so, we only do it
            // if this flag is set. (It defaults to set, we'll clear it if work
            // appears below.)
            let mut should_sleep = true;

            // TX engine. We continue moving bytes while these three conditions
            // hold:
            // - More bytes need to be sent.
            // - Permits are available.
            // - The TX FIFO has space.
            while tx_count < overall_len
                && tx_permits > 0
                && self.spi.can_tx_frame()
            {
                // The next byte to TX will come from the caller, if we haven't
                // run off the end of their lease, or the fixed padding byte if
                // we have.
                let byte = if let Some(txbuf) = &mut tx {
                    if let Some(b) = txbuf.read() {
                        b
                    } else {
                        // We've hit the end of the lease. Stop checking.
                        tx = None;
                        0
                    }
                } else {
                    0
                };

                ringbuf_entry!(Trace::Tx(byte));
                self.spi.send8(byte);
                tx_count += 1;

                // Consume one TX permit to make sure we don't overrun the RX
                // fifo.
                tx_permits -= 1;

                if tx_permits == 0 || tx_count == overall_len {
                    // We're either done, or we need to idle until the RX engine
                    // catches up. Either way, stop generating interrupts.
                    self.spi.disable_can_tx_interrupt();
                }

                // We don't adjust should_sleep in the TX engine because, if we
                // leave this loop, we've done all the TX work we can -- and
                // we're about to check for RX work unconditionally below. So,
                // from the perspective of the TX engine, should_sleep is always
                // true at this point, and the RX engine gets to make the final
                // decision.
            }

            // Drain bytes from the RX FIFO.
            while self.spi.can_rx_byte() {
                // We didn't check rx_count < overall_len above because, if we
                // got to that point, it would mean the SPI hardware gave us
                // more bytes than we sent. This would be bad. And so, we'll
                // detect that condition aggressively:
                if rx_count >= overall_len {
                    panic!();
                }

                // Pull byte from RX FIFO.
                let b = self.spi.recv8();
                ringbuf_entry!(Trace::Rx(b));
                rx_count += 1;

                // Allow another byte to be inserted in the TX FIFO.
                tx_permits += 1;

                // Deposit the byte if we're still within the bounds of the
                // caller's incoming lease.
                if let Some(rx_reader) = &mut rx {
                    if rx_reader.write(b).is_err() {
                        // We're off the end. Stop checking.
                        rx = None;
                    }
                }

                // By releasing a TX permit, we might have unblocked the TX
                // engine. We can detect this when tx_permits goes 0->1. If this
                // occurs, we should turn its interrupt back on, but only if
                // it's still working.
                if tx_permits == 1 && tx_count < overall_len {
                    self.spi.enable_can_tx_interrupt();
                }

                // We've done some work, which means some time has elapsed,
                // which means it's possible that room in the TX FIFO has opened
                // up. So, let's not sleep.
                should_sleep = false;
            }

            if should_sleep {
                ringbuf_entry!(Trace::WaitISR(self.spi.read_status()));

                if self.spi.check_overrun() {
                    panic!();
                }

                // Allow the controller interrupt to post to our
                // notification set.
                sys_irq_control(IRQ_MASK, true);
                // Wait for our notification set to get, well, set. We ignore
                // the result of this because an error would mean the kernel
                // violated the ABI, which we can't usefully respond to.
                let _ = sys_recv_closed(&mut [], IRQ_MASK, TaskId::KERNEL);
            }
        }

        // Because we've pulled all the bytes from the RX FIFO, we should be
        // able to observe the EOT condition here.
        if !self.spi.check_eot() {
            panic!();
        }
        self.spi.clear_eot();

        // Wrap up the transfer and restore things to a reasonable
        // state.
        self.spi.end();

        // Deassert (set) CS, if we asserted it in the first place.
        if !cs_override {
            self.sys
                .gpio_set_reset(device.cs.port, device.cs.pin_mask, 0)
                .unwrap();
        }

        Ok(())
    }
}

fn deactivate_mux_option(opt: &SpiMuxOption, gpio: &sys_api::Sys) {
    // Drive all output pins low.
    for &(pins, _af) in opt.outputs {
        gpio.gpio_set_reset(pins.port, 0, pins.pin_mask).unwrap();
        gpio.gpio_configure(
            pins.port,
            pins.pin_mask,
            sys_api::Mode::Output,
            sys_api::OutputType::PushPull,
            sys_api::Speed::Low,
            sys_api::Pull::None,
            sys_api::Alternate::AF0, // doesn't matter in GPIO mode
        )
        .unwrap();
    }
    // Switch input pin away from SPI peripheral to a GPIO input, which makes it
    // Hi-Z.
    gpio.gpio_configure(
        opt.input.0.port,
        opt.input.0.pin_mask,
        sys_api::Mode::Input,
        sys_api::OutputType::PushPull, // doesn't matter
        sys_api::Speed::High,          // doesn't matter
        sys_api::Pull::None,
        sys_api::Alternate::AF0, // doesn't matter
    )
    .unwrap();
}

fn activate_mux_option(
    opt: &SpiMuxOption,
    gpio: &sys_api::Sys,
    spi: &spi_core::Spi,
) {
    // Apply the data line swap if requested.
    spi.set_data_line_swap(opt.swap_data);
    // Switch all outputs to the SPI peripheral.
    for &(pins, af) in opt.outputs {
        gpio.gpio_configure(
            pins.port,
            pins.pin_mask,
            sys_api::Mode::Alternate,
            sys_api::OutputType::PushPull,
            sys_api::Speed::Low,
            sys_api::Pull::None,
            af,
        )
        .unwrap();
    }
    // And the input too.
    gpio.gpio_configure(
        opt.input.0.port,
        opt.input.0.pin_mask,
        sys_api::Mode::Alternate,
        sys_api::OutputType::PushPull, // doesn't matter
        sys_api::Speed::High,          // doesn't matter
        sys_api::Pull::None,
        opt.input.1,
    )
    .unwrap();
}

//////////////////////////////////////////////////////////////////////////////
// Board-peripheral-server configuration matrix
//
// The configurable bits for a given board and controller combination are in the
// ServerConfig struct. We use conditional compilation below to select _one_
// instance of this struct in a const called `CONFIG`.

/// Rolls up all the configuration options for this server on a given board and
/// controller.
#[derive(Copy, Clone)]
struct ServerConfig {
    /// Pointer to this controller's register block. Don't let the `spi1` fool
    /// you, they all have that type. This needs to match a peripheral in your
    /// task's `uses` list for this to work.
    registers: *const device::spi1::RegisterBlock,
    /// Name for the peripheral as far as the RCC is concerned.
    peripheral: sys_api::Peripheral,
    /// We allow for an individual SPI controller to be switched between several
    /// physical sets of pads. The mux options for a given server configuration
    /// are numbered from 0 and correspond to this slice.
    mux_options: &'static [SpiMuxOption],
    /// We keep track of a fixed set of devices per SPI controller, which each
    /// have an associated routing (from `mux_options`) and CS pin.
    devices: &'static [DeviceDescriptor],
}

/// A routing of the SPI controller onto pins.
#[derive(Copy, Clone, Debug)]
struct SpiMuxOption {
    /// A list of config changes to apply to activate the output pins of this
    /// mux option. This is a list because some mux options are spread across
    /// multiple ports, or (in at least one case) the pins in the same port
    /// require different AF numbers to work.
    ///
    /// To disable the mux, we'll force these pins low. This is correct for SPI
    /// mode 0/1 but not mode 2/3; fortunately we currently don't support mode
    /// 2/3, so we can simplify.
    outputs: &'static [(PinSet, sys_api::Alternate)],
    /// A list of config changes to apply to activate the input pins of this mux
    /// option. This is _not_ a list because there's only one such pin, CIPO.
    ///
    /// To disable the mux, we'll switch this pin to HiZ.
    input: (PinSet, sys_api::Alternate),
    /// Swap data lines?
    swap_data: bool,
}

#[derive(Copy, Clone, Debug)]
struct PinSet {
    port: sys_api::Port,
    pin_mask: u16,
}

/// Information about one device attached to the SPI controller.
#[derive(Copy, Clone, Debug)]
struct DeviceDescriptor {
    /// To reach this device, the SPI controller has to be muxed onto the
    /// correct physical circuit. This gives the index of the right choice in
    /// the server's configured `SpiMuxOption` array.
    mux_index: usize,
    /// Where the CS pin is. While this is a `PinSet`, it should only have one
    /// pin in it, and we check this at startup.
    cs: PinSet,
    /// Clock divider to apply while speaking with this device. Yes, this says
    /// spi1 no matter which SPI block we're in charge of.
    clock_divider: device::spi1::cfg1::MBR_A,
}

/// Any impl of ServerConfig for Server has to pass these tests at startup.
fn check_server_config() {
    // TODO some of this could potentially be moved into const fns for building
    // the tree, and thus to compile time ... if we could assert in const fns.
    //
    // That said, because this is analyzing constants, if the checks _pass_ this
    // should disappear at compilation.

    assert!(!CONFIG.registers.is_null()); // let's start off easy.

    // Mux options must be provided.
    assert!(!CONFIG.mux_options.is_empty());
    for muxopt in CONFIG.mux_options {
        // Each mux option must contain at least one output config record.
        assert!(!muxopt.outputs.is_empty());
        let mut total_pins = 0;
        for (pinset, _af) in muxopt.outputs {
            // Each config must apply to at least one pin.
            assert!(pinset.pin_mask != 0);
            // If this is the same port as the input pin, it must not _include_
            // the input pin.
            if pinset.port == muxopt.input.0.port {
                assert!(pinset.pin_mask & muxopt.input.0.pin_mask == 0);
            }
            // We're counting how many total pins are controlled here.
            total_pins += pinset.pin_mask.count_ones();
        }
        // There should be two affected output pins (COPI, SCK). This check
        // prevents people from being clever and trying to mux SPI to two
        // locations simultaneously, which Does Not Work. It also catches
        // mistakenly including CIPO in the outputs set.
        assert!(total_pins == 2);
        // There should be exactly one pin in the input set.
        assert!(muxopt.input.0.pin_mask.count_ones() == 1);
    }
    // At least one device must be defined.
    assert!(!CONFIG.devices.is_empty());
    for dev in CONFIG.devices {
        // Mux index must be valid.
        assert!(dev.mux_index < CONFIG.mux_options.len());
        // CS pin must designate _exactly one_ pin in its mask.
        assert!(dev.cs.pin_mask.is_power_of_two());
    }
}

include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
include!(concat!(env!("OUT_DIR"), "/spi_config.rs"));
