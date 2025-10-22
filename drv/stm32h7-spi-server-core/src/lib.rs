// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Library that extracts the logic of owning a STM32H7 SPI peripheral.
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
use idol_runtime::{BufReader, BufWriter, ClientError, RequestError};
use ringbuf::*;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use userlib::*;

use drv_stm32h7_spi as spi_core;
use drv_stm32xx_sys_api as sys_api;
use sys_api::PinSet;

use core::{cell::Cell, convert::Infallible};

////////////////////////////////////////////////////////////////////////////////

/// The `SpiServerCore` owns a particular SPI peripheral and allows us to talk
/// to it.
///
/// It can be used as the core of a Hubris server (`drv-stm32h7-spi-server`),
/// *or* embedded directly into an application that has exclusive use of that
/// particular SPI peripheral.  The latter reduces task count and IPC overhead.
///
/// As such, it implements the `SpiServer` trait, which makes it... a little
/// funky.  It has to be `Clone`, because `SpiDevice` wants to own a copy of it
/// (by value); this means that state has to be provided by the caller.  In
/// addition, it shouldn't take a lifetime (because `Spi` doesn't have a
/// lifetime), meaning that storage must be static.
///
/// You probably want to use `declare_spi_core!`, which creates both the SPI
/// core and the interior-mutable static storage associated with it.
#[derive(Clone)]
pub struct SpiServerCore {
    spi: spi_core::Spi,
    sys: sys_api::Sys,
    irq_mask: u32,
    lock_holder: &'static Cell<Option<LockState>>, // used by Idol server
    current_mux_index: &'static Cell<usize>,
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    Start(#[count(children)] SpiOperation, (u16, u16)),
    Tx(u8),
    Rx(u8),
    WaitISR(u32),
}

counted_ringbuf!(Trace, 64, Trace::None);

#[derive(Copy, Clone, Debug)]
pub struct LockState {
    task: TaskId,
    device_index: usize,
}

/// Errors returned by [`SpiServerCore::read`], [`SpiServerCore::write`], and
/// [`SpiServerCore::exchange`].
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum TransferError {
    /// Transfer size is 0 or exceeds maximum
    BadTransferSize = 1,

    /// Attempt to operate device N when there is no device N, or an attempt to
    /// operate on _any other_ device when you've locked the controller to one.
    ///
    /// This is almost certainly a programming error on the client side.
    BadDevice = 2,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub struct LockError(());

impl From<TransferError> for RequestError<SpiError> {
    fn from(value: TransferError) -> Self {
        match value {
            TransferError::BadTransferSize => {
                RequestError::Runtime(SpiError::BadTransferSize)
            }
            TransferError::BadDevice => {
                RequestError::Fail(ClientError::BadMessageContents)
            }
        }
    }
}

impl From<LockError> for RequestError<Infallible> {
    fn from(_: LockError) -> RequestError<Infallible> {
        RequestError::Fail(ClientError::BadMessageContents)
    }
}

////////////////////////////////////////////////////////////////////////////////

impl SpiServerCore {
    pub fn init(
        sys: sys_api::Sys,
        irq_mask: u32,
        lock_holder: &'static Cell<Option<LockState>>, // used by Idol server
        current_mux_index: &'static Cell<usize>,
    ) -> Self {
        check_server_config();

        let registers = unsafe { &*CONFIG.registers };

        sys.enable_clock(CONFIG.peripheral);
        sys.enter_reset(CONFIG.peripheral);
        sys.leave_reset(CONFIG.peripheral);
        let mut spi = spi_core::Spi::from(registers);

        // This should correspond to '0' in the standard SPI parlance
        spi.initialize(
            device::spi1::cfg1::MBR_A::Div64,
            8,
            device::spi1::cfg2::COMM_A::FullDuplex,
            device::spi1::cfg2::LSBFRST_A::Msbfirst,
            device::spi1::cfg2::CPHA_A::FirstEdge,
            device::spi1::cfg2::CPOL_A::IdleLow,
            device::spi1::cfg2::SSOM_A::Asserted,
        );

        // Configure all devices' CS pins to be deasserted (set).
        // We leave them in GPIO output mode from this point forward.
        for device in CONFIG.devices {
            for pin in device.cs {
                sys.gpio_set(*pin);
                sys.gpio_configure_output(
                    *pin,
                    sys_api::OutputType::PushPull,
                    sys_api::Speed::Low,
                    sys_api::Pull::None,
                );
            }
        }

        // Initially, configure mux 0. This keeps us from having to deal with a "no
        // mux selected" state.
        //
        // Note that the config check routine above ensured that there _is_ a mux
        // option 0.
        //
        // We deactivate before activate to avoid pin clash if we previously crashed
        // with one of these activated.
        current_mux_index.set(0);
        for opt in &CONFIG.mux_options[1..] {
            deactivate_mux_option(opt, &sys);
        }
        activate_mux_option(
            &CONFIG.mux_options[current_mux_index.get()],
            &sys,
            &spi,
        );

        Self {
            spi,
            sys,
            irq_mask,
            lock_holder,
            current_mux_index,
        }
    }

    pub fn recv_source(&self) -> Option<userlib::TaskId> {
        self.lock_holder.get().map(|s| s.task)
    }

    pub fn closed_recv_fail(&self) {
        // Welp, someone had asked us to lock and then died. Release the lock
        self.lock_holder.set(None);
    }

    pub fn read<'b, BufWrite: BufWriter<'b>>(
        &self,
        device_index: u8,
        dest: BufWrite,
    ) -> Result<(), TransferError> {
        self.ready_writey::<&[u8], _>(
            SpiOperation::read,
            device_index,
            None,
            Some(dest),
        )
    }

    pub fn write<'b, BufRead: BufReader<'b>>(
        &self,
        device_index: u8,
        src: BufRead,
    ) -> Result<(), TransferError> {
        self.ready_writey::<_, &mut [u8]>(
            SpiOperation::write,
            device_index,
            Some(src),
            None,
        )
    }

    pub fn exchange<'b, BufRead: BufReader<'b>, BufWrite: BufWriter<'b>>(
        &self,
        device_index: u8,
        src: BufRead,
        dest: BufWrite,
    ) -> Result<(), TransferError> {
        self.ready_writey(
            SpiOperation::exchange,
            device_index,
            Some(src),
            Some(dest),
        )
    }

    pub fn lock(
        &self,
        sender: TaskId,
        devidx: u8,
        cs_state: CsState,
    ) -> Result<(), LockError> {
        let cs_asserted = cs_state == CsState::Asserted;
        let devidx = usize::from(devidx);

        // If we are locked there are more rules:
        if let Some(lockstate) = &self.lock_holder.get() {
            // The fact that we received this message _at all_ means
            // that the sender matched our closed receive, but just
            // in case we have a server logic bug, let's check.
            assert!(lockstate.task == sender);
            // The caller is not allowed to change the device index
            // once locked.
            if lockstate.device_index != devidx {
                return Err(LockError(()));
            }
        }

        // OK! We are either (1) just locking now or (2) processing
        // a legal state change from the same sender.

        // Reject out-of-range devices.
        let device = CONFIG.devices.get(devidx).ok_or(LockError(()))?;

        for pin in device.cs {
            // If we're asserting CS, we want to *reset* the pin. If
            // we're not, we want to *set* it. Because CS is active low.
            if cs_asserted {
                self.sys.gpio_reset(*pin);
            } else {
                self.sys.gpio_set(*pin);
            }
        }

        self.lock_holder.set(Some(LockState {
            task: sender,
            device_index: devidx,
        }));
        Ok(())
    }

    pub fn release(&self, sender: TaskId) -> Result<(), LockError> {
        if let Some(lockstate) = &self.lock_holder.get() {
            // The fact that we were able to receive this means we
            // should be locked by the sender...but double check.
            assert!(lockstate.task == sender);

            let device = &CONFIG.devices[lockstate.device_index];

            for pin in device.cs {
                // Deassert CS. If it wasn't asserted, this is a no-op.
                // If it was, this fixes that.
                self.sys.gpio_set(*pin);
            }

            self.lock_holder.set(None);
            Ok(())
        } else {
            Err(LockError(()))
        }
    }

    fn ready_writey<'b, BufRead: BufReader<'b>, BufWrite: BufWriter<'b>>(
        &self,
        op: SpiOperation,
        device_index: u8,
        mut tx: Option<BufRead>,
        mut rx: Option<BufWrite>,
    ) -> Result<(), TransferError> {
        let device_index = usize::from(device_index);

        // If we are locked, check that the caller isn't mistakenly
        // addressing the wrong device.
        if let Some(lockstate) = &self.lock_holder.get() {
            if lockstate.device_index != device_index {
                return Err(TransferError::BadDevice);
            }
        }

        // Reject out-of-range devices.
        let device = CONFIG
            .devices
            .get(device_index)
            .ok_or(TransferError::BadDevice)?;

        // At least one lease must be provided. A failure here indicates that
        // the server stub calling this common routine is broken, not a client
        // mistake.
        if tx.is_none() && rx.is_none() {
            panic!();
        }

        // Get the required transfer lengths in the src and dest directions.
        //
        // Sizes that overflow a u16 are invalid and we reject them
        let src_len: u16 = tx
            .as_ref()
            .map(|tx| tx.remaining_size())
            .unwrap_or(0)
            .try_into()
            .map_err(|_| TransferError::BadTransferSize)?;
        let dest_len: u16 = rx
            .as_ref()
            .map(|rx| rx.remaining_size())
            .unwrap_or(0)
            .try_into()
            .map_err(|_| TransferError::BadTransferSize)?;
        let overall_len = src_len.max(dest_len);

        // Zero-byte SPI transactions don't make sense and we'll
        // decline them.
        if overall_len == 0 {
            return Err(TransferError::BadTransferSize);
        }

        // We have a reasonable-looking request containing reasonable-looking
        // lease(s). This is our commit point.
        ringbuf_entry!(Trace::Start(op, (src_len, dest_len)));

        // Switch the mux to the requested port.
        let current_mux_index = self.current_mux_index.get();
        if device.mux_index != current_mux_index {
            deactivate_mux_option(
                &CONFIG.mux_options[current_mux_index],
                &self.sys,
            );
            activate_mux_option(
                &CONFIG.mux_options[device.mux_index],
                &self.sys,
                &self.spi,
            );
            // Remember this for later to avoid unnecessary
            // switching.
            self.current_mux_index.set(device.mux_index);
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

        // Enable interrupt on the conditions we're interested in.
        self.spi.enable_transfer_interrupts();

        self.spi.clear_eot();

        // We're doing this! Check if we need to control CS.
        let cs_override = self.lock_holder.get().is_some();
        if !cs_override {
            for pin in device.cs {
                self.sys.gpio_reset(*pin);
            }
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
                    // TODO: lint is buggy in 2024-04-04 toolchain, retest later
                    #[allow(clippy::manual_unwrap_or_default)]
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
                sys_irq_control(self.irq_mask, true);
                // Wait for our notification set to get, well, set.
                sys_recv_notification(self.irq_mask);
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
            for pin in device.cs {
                self.sys.gpio_set(*pin);
            }
        }

        Ok(())
    }
}

fn deactivate_mux_option(opt: &SpiMuxOption, gpio: &sys_api::Sys) {
    // Drive all output pins low.
    for &(pins, _af) in opt.outputs {
        gpio.gpio_reset(pins);
        gpio.gpio_configure_output(
            pins,
            sys_api::OutputType::PushPull,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );
    }
    // Switch input pin away from SPI peripheral to a GPIO input, which makes it
    // Hi-Z.
    gpio.gpio_configure_input(opt.input.0, sys_api::Pull::None);
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
        );
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
    );
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

/// Information about one device attached to the SPI controller.
#[derive(Copy, Clone, Debug)]
struct DeviceDescriptor {
    /// To reach this device, the SPI controller has to be muxed onto the
    /// correct physical circuit. This gives the index of the right choice in
    /// the server's configured `SpiMuxOption` array.
    mux_index: usize,
    /// Where the CS pin is. While this is a `PinSet`, it should only have one
    /// pin in it, and we check this at startup.
    cs: &'static [PinSet],
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

        for pin in dev.cs {
            // A CS pin must designate _exactly one_ pin in its mask.
            assert!(pin.pin_mask.is_power_of_two());
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

impl SpiServer for SpiServerCore {
    fn exchange(
        &self,
        device_index: u8,
        src: &[u8],
        dest: &mut [u8],
    ) -> Result<(), SpiError> {
        SpiServerCore::exchange(self, device_index, src, dest).map_err(|e| {
            match e {
                // If the SPI server was in a remote task, this case would
                // return a reply-fault; therefore, panicking the task when the
                // SPI driver is local to that task is appropriate.
                TransferError::BadDevice => panic!(),
                TransferError::BadTransferSize => SpiError::BadTransferSize,
            }
        })
    }

    fn write(&self, device_index: u8, src: &[u8]) -> Result<(), SpiError> {
        SpiServerCore::write(self, device_index, src).map_err(|e| match e {
            // If the SPI server was in a remote task, this case would
            // return a reply-fault; therefore, panicking the task when the
            // SPI driver is local to that task is appropriate.
            TransferError::BadDevice => panic!(),
            TransferError::BadTransferSize => SpiError::BadTransferSize,
        })
    }

    fn read(&self, device_index: u8, dest: &mut [u8]) -> Result<(), SpiError> {
        SpiServerCore::read(self, device_index, dest).map_err(|e| match e {
            // If the SPI server was in a remote task, this case would
            // return a reply-fault; therefore, panicking the task when the
            // SPI driver is local to that task is appropriate.
            TransferError::BadDevice => panic!(),
            TransferError::BadTransferSize => SpiError::BadTransferSize,
        })
    }

    fn lock(
        &self,
        device_index: u8,
        cs_state: CsState,
    ) -> Result<(), idol_runtime::ServerDeath> {
        // When someone is using the SpiServerCore directly (rather than through
        // RPC), we use TaskId::UNBOUND as the locking task.
        SpiServerCore::lock(self, TaskId::UNBOUND, device_index, cs_state)
            .unwrap_lite();
        Ok(())
    }

    fn release(&self) -> Result<(), idol_runtime::ServerDeath> {
        SpiServerCore::release(self, TaskId::UNBOUND).unwrap_lite();
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////

pub use mutable_statics::mutable_statics as __mutable_statics_reexport;

#[macro_export]
macro_rules! declare_spi_core {
    ($sys:expr, $irq_mask:expr) => {{
        let (lock_holder, current_mux_index) =
            $crate::__mutable_statics_reexport!(
                static mut LOCK_HOLDER: [core::cell::Cell<
                    Option<$crate::LockState>,
                >; 1] = [|| core::cell::Cell::new(None); _];
                static mut MUX_INDEX: [core::cell::Cell<usize>; 1] =
                    [|| core::cell::Cell::new(0); _];
            );
        $crate::SpiServerCore::init(
            $sys,
            $irq_mask,
            &lock_holder[0],
            &current_mux_index[0],
        )
    }}
}

////////////////////////////////////////////////////////////////////////////////

include!(concat!(env!("OUT_DIR"), "/spi_config.rs"));
