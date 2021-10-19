//! Server task for the STM32H7 SPI peripheral.
//!
//! Currently this hardcodes the clock rate.
//!
//! See the `spi-api` crate for the protocol being implemented here.

#![no_std]
#![no_main]

use drv_spi_api::*;
use ringbuf::*;
use stm32h7::stm32h743 as device;
use userlib::*;

use drv_stm32h7_gpio_api as gpio_api;
use drv_stm32h7_rcc_api as rcc_api;
use drv_stm32h7_spi as spi_core;

task_slot!(RCC, rcc_driver);
task_slot!(GPIO, gpio_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Start(Operation, (usize, usize)),
    Tx(usize, u8),
    Rx(usize, u8),
    None,
}

ringbuf!(Trace, 64, Trace::None);

const IRQ_MASK: u32 = 1;

#[derive(Copy, Clone, Debug)]
struct LockState {
    task: TaskId,
    device_index: usize,
    cs_state: CsState,
}

#[export_name = "main"]
fn main() -> ! {
    check_server_config();

    let rcc_driver = rcc_api::Rcc::from(RCC.get_task_id());

    let registers = unsafe { &*CONFIG.registers };

    rcc_driver.enable_clock(CONFIG.peripheral);
    rcc_driver.leave_reset(CONFIG.peripheral);
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

    let gpio_driver = gpio_api::Gpio::from(GPIO.get_task_id());

    // Configure all devices' CS pins to be deasserted (set).
    // We leave them in GPIO output mode from this point forward.
    for device in CONFIG.devices {
        gpio_driver
            .set_reset(device.cs.port, device.cs.pin_mask, 0)
            .unwrap();
        gpio_driver
            .configure(
                device.cs.port,
                device.cs.pin_mask,
                gpio_api::Mode::Output,
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::High,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF1, // doesn't matter in GPIO mode
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
    let mut current_mux_index = 0;
    for opt in &CONFIG.mux_options[1..] {
        deactivate_mux_option(&opt, &gpio_driver);
    }
    activate_mux_option(&CONFIG.mux_options[0], &gpio_driver, &spi);

    // If we get a lock request, we'll update this with the task ID. We'll then
    // use it to decide between open and closed receive.
    let mut lock_holder: Option<LockState> = None;
    loop {
        // Note: we process the result of recv at the bottom of the loop.
        let rr = hl::recv_from_without_notification(
            // If we are locked, pass Some(taskid) to do a closed receive.
            // Otherwise pass None to do an open receive.
            lock_holder.map(|state| state.task),
            // Our longest operation is two bytes (lock).
            &mut [0; 2],
            |op, msg| match op {
                Operation::Lock => {
                    let (&[devidx, cs_state], caller) =
                        msg.fixed::<[u8; 2], ()>().ok_or(SpiError::BadArg)?;
                    let cs_state = if cs_state == 0 {
                        CsState::NotAsserted
                    } else {
                        CsState::Asserted
                    };
                    let cs_asserted = cs_state == CsState::Asserted;
                    let devidx = usize::from(devidx);

                    // If we are locked there are more rules:
                    if let Some(lockstate) = &lock_holder {
                        // The fact that we received this message _at all_ means
                        // that the sender matched our closed receive, but just
                        // in case we have a server logic bug, let's check.
                        assert!(lockstate.task == caller.task_id());
                        // The caller is not allowed to change the device index
                        // once locked.
                        if lockstate.device_index != devidx {
                            return Err(SpiError::BadDevice);
                        }
                    }

                    // OK! We are either (1) just locking now or (2) processing
                    // a legal state change from the same sender.

                    // Reject out-of-range devices.
                    let device = CONFIG
                        .devices
                        .get(devidx)
                        .ok_or(SpiError::BadDevice)?;

                    // If we're asserting CS, we want to *reset* the pin. If
                    // we're not, we want to *set* it. Because CS is active low.
                    let pin_mask = device.cs.pin_mask;
                    gpio_driver
                        .set_reset(
                            device.cs.port,
                            if cs_asserted { 0 } else { pin_mask },
                            if cs_asserted { pin_mask } else { 0 },
                        )
                        .unwrap();
                    lock_holder = Some(LockState {
                        task: caller.task_id(),
                        device_index: devidx,
                        cs_state,
                    });
                    caller.reply(());
                    Ok(())
                }
                Operation::Release => {
                    let ((), caller) = msg.fixed().ok_or(SpiError::BadArg)?;
                    if let Some(lockstate) = &lock_holder {
                        // The fact that we were able to receive this means we
                        // should be locked by the sender...but double check.
                        assert!(lockstate.task == caller.task_id());

                        let device = &CONFIG.devices[lockstate.device_index];

                        // Deassert CS. If it wasn't asserted, this is a no-op.
                        // If it was, this fixes that.
                        gpio_driver
                            .set_reset(device.cs.port, device.cs.pin_mask, 0)
                            .unwrap();
                        lock_holder = None;
                        caller.reply(());
                        Ok(())
                    } else {
                        Err(SpiError::NothingToRelease)
                    }
                }
                // And now, the readey-writey options
                Operation::Exchange | Operation::Read | Operation::Write => {
                    // We can take varying numbers of leases, so we'll do lease
                    // verification ourselves just below.
                    let lease_count = msg.lease_count();
                    let (&device_index, caller) =
                        msg.fixed::<u8, ()>().ok_or(SpiError::BadArg)?;
                    let device_index = usize::from(device_index);

                    // If we are locked, check that the caller isn't mistakenly
                    // addressing the wrong device.
                    if let Some(lockstate) = &lock_holder {
                        if lockstate.device_index != device_index {
                            return Err(SpiError::BadDevice);
                        }
                    }

                    // Reject out-of-range devices.
                    let device = CONFIG
                        .devices
                        .get(device_index)
                        .ok_or(SpiError::BadDevice)?;

                    // Inspect the message and generate two `Option<Borrow>`s
                    // and a transfer length. Note: the two borrows may refer to
                    // the same buffer! See the `1` case below for details.
                    let (data_src, data_dst, xfer_len) = match lease_count {
                        1 => {
                            // Caller has provided a single lease, which must
                            // have different attributes depending on what
                            // operation they've requested.
                            let borrow = caller.borrow(0);
                            let info =
                                borrow.info().ok_or(SpiError::BadLeaseArg)?;

                            // Note that the attributes _we_ require are the
                            // inverse of the sense of the SPI operation, e.g.
                            // to read from SPI we must be able to _write_ the
                            // lease, and vice versa.
                            let required_attributes = match op {
                                Operation::Read => LeaseAttributes::WRITE,
                                Operation::Write => LeaseAttributes::READ,
                                _ => {
                                    LeaseAttributes::WRITE
                                        | LeaseAttributes::READ
                                }
                            };

                            if !info.attributes.contains(required_attributes) {
                                return Err(SpiError::BadLeaseAttributes);
                            }

                            let read_borrow = if op.is_write() {
                                Some(borrow.clone())
                            } else {
                                None
                            };
                            let write_borrow =
                                if op.is_read() { Some(borrow) } else { None };

                            (read_borrow, write_borrow, (info.len, info.len))
                        }
                        2 if op == Operation::Exchange => {
                            // Caller has provided two leases, the first as a
                            // data source and the second as a data sink. This
                            // is only legal if we are both transmitting and
                            // receiving. The transmit buffer cannot be larger
                            // than the receive buffer; for any bytes for which
                            // the receive buffer exceeds the transmit buffer,
                            // a zero byte will be put on the wire.
                            let src_borrow = caller.borrow(0);
                            let src_info =
                                src_borrow.info().ok_or(SpiError::BadSource)?;

                            if !src_info
                                .attributes
                                .contains(LeaseAttributes::READ)
                            {
                                return Err(SpiError::BadSourceAttributes);
                            }

                            let dst_borrow = caller.borrow(1);
                            let dst_info =
                                dst_borrow.info().ok_or(SpiError::BadSink)?;

                            if !dst_info
                                .attributes
                                .contains(LeaseAttributes::WRITE)
                            {
                                return Err(SpiError::BadSinkAttributes);
                            }

                            if dst_info.len < src_info.len {
                                return Err(SpiError::ShortSinkLength);
                            }

                            (
                                Some(src_borrow),
                                Some(dst_borrow),
                                (dst_info.len, src_info.len),
                            )
                        }
                        _ => return Err(SpiError::BadLeaseCount),
                    };

                    // That routine should have returned at least one borrow.
                    // Here's an assert that takes fewer text bytes than assert.
                    if data_src.is_none() && data_dst.is_none() {
                        panic!()
                    }

                    // Due to driver limitations we will only move up to 64kiB
                    // per transaction. It would be worth lifting this
                    // limitation, maybe. Doing so would require managing data
                    // in 64kiB chunks (because the peripheral is 16-bit) and
                    // using the "reload" facility on the peripheral.
                    //
                    // Zero-byte SPI transactions don't make sense and we'll
                    // decline them.
                    if xfer_len.0 == 0 || xfer_len.0 >= 0x1_0000 {
                        return Err(SpiError::BadTransferSize);
                    }

                    // We have a reasonable-looking request containing (a)
                    // reasonable-looking lease(s). This is our commit point.
                    ringbuf_entry!(Trace::Start(op, xfer_len));

                    // Switch the mux to the requested port.
                    if device.mux_index != current_mux_index {
                        deactivate_mux_option(
                            &CONFIG.mux_options[current_mux_index],
                            &gpio_driver,
                        );
                        activate_mux_option(
                            &CONFIG.mux_options[device.mux_index],
                            &gpio_driver,
                            &spi,
                        );
                        // Remember this for later to avoid unnecessary
                        // switching.
                        current_mux_index = device.mux_index;
                    }

                    // Make sure SPI is on.
                    spi.enable(xfer_len.0 as u16);

                    // Load transfer count and start the state machine. At this
                    // point we _have_ to move the specified number of bytes
                    // through (or explicitly cancel, but we don't).
                    spi.start();

                    // As you might expect, we will work from byte 0 to the end
                    // of each buffer. There are two complications:
                    //
                    // 1. Transmit and receive can be at different positions --
                    //    transmit will tend to lead receive, because the SPI
                    //    unit contains FIFOs.
                    //
                    // 2. We're only keeping track of position in the buffers
                    //    we're using: both tx and rx are `Option<(Borrow,
                    //    usize)>`.

                    // Tack a position field onto whichever borrows actually
                    // exist.
                    let mut tx = data_src.map(|borrow| (borrow, 0));
                    let mut rx = data_dst.map(|borrow| (borrow, 0));

                    // Enable interrupt on the conditions we're interested in.
                    spi.enable_transfer_interrupts();

                    spi.clear_eot();

                    // We're doing this! Check if we need to control CS.
                    let cs_override = lock_holder.is_some();
                    if !cs_override {
                        gpio_driver
                            .set_reset(device.cs.port, 0, device.cs.pin_mask)
                            .unwrap();
                    }

                    // While work remains, we'll attempt to move up to one byte
                    // in each direction, sleeping if we can do neither.
                    while tx.is_some() || rx.is_some() {
                        // Entering RECV to check for interrupts is not free, so
                        // we only do it if we've filled the TX FIFO and emptied
                        // the RX and repeating this loop would just burn power
                        // and CPU. If there is any potential value to repeating
                        // the loop immediately, we'll set this flag.
                        let mut made_progress = false;

                        if let Some((tx_data, tx_pos)) = &mut tx {
                            if spi.can_tx_frame() {
                                // If our position is less than our tx len,
                                // transfer a byte from caller to TX FIFO --
                                // otherwise put a dummy byte on the wire
                                let byte: u8 = if *tx_pos < xfer_len.1 {
                                    tx_data
                                        .read_at(*tx_pos)
                                        .ok_or(SpiError::BadSourceByte)?
                                } else {
                                    0u8
                                };

                                ringbuf_entry!(Trace::Tx(*tx_pos, byte));
                                spi.send8(byte);
                                *tx_pos += 1;

                                // If we have _just_ finished...
                                if *tx_pos == xfer_len.0 {
                                    // We will finish transmitting well before
                                    // we're done receiving, so stop getting
                                    // interrupt notifications for transmit
                                    // space available during that time.
                                    spi.disable_can_tx_interrupt();
                                    tx = None;
                                }

                                made_progress = true;
                            }
                        }

                        if let Some((rx_data, rx_pos)) = &mut rx {
                            if spi.can_rx_byte() {
                                // Transfer byte from RX FIFO to caller.
                                let r = spi.recv8();
                                rx_data
                                    .write_at(*rx_pos, r)
                                    .ok_or(SpiError::BadSinkByte)?;
                                ringbuf_entry!(Trace::Rx(*rx_pos, r));
                                *rx_pos += 1;

                                if *rx_pos == xfer_len.0 {
                                    rx = None;
                                }

                                made_progress = true;
                            }
                        }

                        if !made_progress {
                            // Allow the controller interrupt to post to our
                            // notification set.
                            sys_irq_control(IRQ_MASK, true);
                            // Wait for our notification set to get, well, set.
                            sys_recv_closed(&mut [], IRQ_MASK, TaskId::KERNEL)
                                .expect("kernel died?");
                        }
                    }

                    // Wait for the final EOT interrupt to ensure we're really
                    // done before returning to the client
                    loop {
                        sys_irq_control(IRQ_MASK, true);
                        sys_recv_closed(&mut [], IRQ_MASK, TaskId::KERNEL)
                            .expect("kernel died?");

                        if spi.check_eot() {
                            spi.clear_eot();
                            break;
                        }
                    }

                    // Wrap up the transfer and restore things to a reasonable
                    // state.
                    spi.end();

                    // Deassert (set) CS.
                    if !cs_override {
                        gpio_driver
                            .set_reset(device.cs.port, device.cs.pin_mask, 0)
                            .unwrap();
                    }

                    // As we're done with the borrows, we can now resume the
                    // caller.
                    caller.reply(());

                    Ok(())
                }
            },
        );

        if rr.is_err() {
            // Welp, someone had asked us to lock and then died. Release the
            // lock.
            lock_holder = None;
        }
    }
}

fn deactivate_mux_option(opt: &SpiMuxOption, gpio: &gpio_api::Gpio) {
    // Drive all output pins low.
    for &(pins, _af) in opt.outputs {
        gpio.set_reset(pins.port, 0, pins.pin_mask).unwrap();
        gpio.configure(
            pins.port,
            pins.pin_mask,
            gpio_api::Mode::Output,
            gpio_api::OutputType::PushPull,
            gpio_api::Speed::High,
            gpio_api::Pull::None,
            gpio_api::Alternate::AF0, // doesn't matter in GPIO mode
        )
        .unwrap();
    }
    // Switch input pin away from SPI peripheral to a GPIO input, which makes it
    // Hi-Z.
    gpio.configure(
        opt.input.0.port,
        opt.input.0.pin_mask,
        gpio_api::Mode::Input,
        gpio_api::OutputType::PushPull, // doesn't matter
        gpio_api::Speed::High,          // doesn't matter
        gpio_api::Pull::None,
        gpio_api::Alternate::AF0, // doesn't matter
    )
    .unwrap();
}

fn activate_mux_option(
    opt: &SpiMuxOption,
    gpio: &gpio_api::Gpio,
    spi: &spi_core::Spi,
) {
    // Apply the data line swap if requested.
    spi.set_data_line_swap(opt.swap_data);
    // Switch all outputs to the SPI peripheral.
    for &(pins, af) in opt.outputs {
        gpio.configure(
            pins.port,
            pins.pin_mask,
            gpio_api::Mode::Alternate,
            gpio_api::OutputType::PushPull,
            gpio_api::Speed::High,
            gpio_api::Pull::None,
            af,
        )
        .unwrap();
    }
    // And the input too.
    gpio.configure(
        opt.input.0.port,
        opt.input.0.pin_mask,
        gpio_api::Mode::Alternate,
        gpio_api::OutputType::PushPull, // doesn't matter
        gpio_api::Speed::High,          // doesn't matter
        gpio_api::Pull::None,
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
    peripheral: rcc_api::Peripheral,
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
    outputs: &'static [(PinSet, gpio_api::Alternate)],
    /// A list of config changes to apply to activate the input pins of this mux
    /// option. This is _not_ a list because there's only one such pin, CIPO.
    ///
    /// To disable the mux, we'll switch this pin to HiZ.
    input: (PinSet, gpio_api::Alternate),
    /// Swap data lines?
    swap_data: bool,
}

#[derive(Copy, Clone, Debug)]
struct PinSet {
    port: gpio_api::Port,
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

cfg_if::cfg_if! {
    //
    // Gemini Bringup Board controllers
    //
    if #[cfg(all(target_board = "gemini-bu-1", feature = "spi2"))] {
        const CONFIG: ServerConfig = ServerConfig {
            registers: device::SPI2::ptr(),
            peripheral: rcc_api::Peripheral::Spi2,
            mux_options: &[
                SpiMuxOption {
                    outputs: &[
                        (
                            PinSet {
                                port: gpio_api::Port::I,
                                pin_mask: (1 << 1) | (1 << 3),
                            },
                            gpio_api::Alternate::AF5,
                        ),
                    ],
                    input: (
                        PinSet {
                            port: gpio_api::Port::I,
                            pin_mask: 1 << 2,
                        },
                        gpio_api::Alternate::AF5,
                    ),
                    swap_data: false,
                },
            ],
            devices: &[
                // Gemini BU SPI2 goes to an unmarked set of pins on an unmarked
                // header, and so does the CS.
                DeviceDescriptor {
                    mux_index: 0,
                    cs: PinSet { port: gpio_api::Port::I, pin_mask: 1 << 0 },
                },
            ],
        };
    } else if #[cfg(all(target_board = "gemini-bu-1", feature = "spi4"))] {
        const CONFIG: ServerConfig = ServerConfig {
            registers: device::SPI4::ptr(),
            peripheral: rcc_api::Peripheral::Spi4,
            mux_options: &[
                // SPI4 is only muxed to one position.
                SpiMuxOption {
                    outputs: &[
                        (
                            PinSet {
                                port: gpio_api::Port::E,
                                pin_mask: (1 << 2) | (1 << 6),
                            },
                            gpio_api::Alternate::AF5,
                        ),
                    ],
                    input: (
                        PinSet {
                            port: gpio_api::Port::E,
                            pin_mask: 1 << 5,
                        },
                        gpio_api::Alternate::AF5,
                    ),
                    swap_data: false,
                },
            ],
            devices: &[
                // The only device is the RoT.
                DeviceDescriptor {
                    mux_index: 0,
                    cs: PinSet { port: gpio_api::Port::E, pin_mask: 1 << 4 },
                },
            ],
        };
    //
    // Glorified Gimletlet controllers
    //
    } else if #[cfg(all(target_board = "gimletlet-2", feature = "spi3"))] {
        const CONFIG: ServerConfig = ServerConfig {
            registers: device::SPI3::ptr(),
            peripheral: rcc_api::Peripheral::Spi3,
            mux_options: &[
                SpiMuxOption {
                    outputs: &[
                        (
                            PinSet {
                                port: gpio_api::Port::C,
                                pin_mask: (1 << 10) | (1 << 12),
                            },
                            gpio_api::Alternate::AF6,
                        ),
                    ],
                    input: (
                        PinSet {
                            port: gpio_api::Port::C,
                            pin_mask: 1 << 11,
                        },
                        gpio_api::Alternate::AF6,
                    ),
                    swap_data: false,
                },
            ],
            devices: &[
                DeviceDescriptor {
                    mux_index: 0,
                    cs: PinSet { port: gpio_api::Port::A, pin_mask: 1 << 15 },
                },
            ],
        };
    } else if #[cfg(all(target_board = "gimletlet-2", feature = "spi4"))] {
        const CONFIG: ServerConfig = ServerConfig {
            registers: device::SPI4::ptr(),
            peripheral: rcc_api::Peripheral::Spi4,
            mux_options: &[
                SpiMuxOption {
                    outputs: &[
                        (
                            PinSet {
                                port: gpio_api::Port::E,
                                pin_mask: (1 << 12) | (1 << 13),
                            },
                            gpio_api::Alternate::AF5,
                        ),
                    ],
                    input: (
                        PinSet {
                            port: gpio_api::Port::E,
                            pin_mask: 1 << 14,
                        },
                        gpio_api::Alternate::AF5,
                    ),
                    swap_data: false,
                },
            ],
            devices: &[
                DeviceDescriptor {
                    mux_index: 0,
                    cs: PinSet { port: gpio_api::Port::E, pin_mask: 1 << 11 },
                },
            ],
        };
    } else if #[cfg(all(target_board = "gimletlet-2", feature = "spi6"))] {
        const CONFIG: ServerConfig = ServerConfig {
            registers: device::SPI6::ptr(),
            peripheral: rcc_api::Peripheral::Spi6,
            mux_options: &[
                SpiMuxOption {
                    outputs: &[
                        (
                            PinSet {
                                port: gpio_api::Port::G,
                                pin_mask: (1 << 13) | (1 << 14),
                            },
                            gpio_api::Alternate::AF5,
                        ),
                    ],
                    input: (
                        PinSet {
                            port: gpio_api::Port::G,
                            pin_mask: 1 << 12,
                        },
                        gpio_api::Alternate::AF5,
                    ),
                    swap_data: false,
                },
            ],
            devices: &[
                DeviceDescriptor {
                    mux_index: 0,
                    cs: PinSet { port: gpio_api::Port::G, pin_mask: 1 << 8 },
                },
            ],
        };
    //
    // Gimlet controllers
    //
    } else if #[cfg(all(target_board = "gimlet-1", feature = "spi2"))] {
        const CONFIG: ServerConfig = ServerConfig {
            registers: device::SPI2::ptr(),
            peripheral: rcc_api::Peripheral::Spi2,
            mux_options: &[
                // Mux option 0 is on port I3:0.
                SpiMuxOption {
                    outputs: &[
                        (
                            PinSet {
                                port: gpio_api::Port::I,
                                pin_mask: (1 << 1) | (1 << 3),
                            },
                            gpio_api::Alternate::AF5,
                        ),
                    ],
                    input: (
                        PinSet {
                            port: gpio_api::Port::I,
                            pin_mask: 1 << 2,
                        },
                        gpio_api::Alternate::AF5,
                    ),
                    swap_data: false,
                },
                // Mux option 1 is on port B15:13.
                SpiMuxOption {
                    outputs: &[
                        (
                            PinSet {
                                port: gpio_api::Port::B,
                                pin_mask: (1 << 13) | (1 << 14),
                            },
                            gpio_api::Alternate::AF5,
                        ),
                    ],
                    input: (
                        PinSet {
                            port: gpio_api::Port::B,
                            pin_mask: 1 << 15,
                        },
                        gpio_api::Alternate::AF5,
                    ),
                    swap_data: true,
                },
            ],
            devices: &[
                // Device 0 is the sequencer logic (design inside U476).
                // Shares port B with the flash and its own programming
                // interface.
                // CS is SP_TO_SEQ_MISC_B.
                DeviceDescriptor {
                    mux_index: 1,
                    cs: PinSet { port: gpio_api::Port::A, pin_mask: 1 << 0 },
                },
                // Device 1 is the U476's iCE40 programming interface.
                // Shares port B with the the other version of U476 and the
                // flash.
                // CS is SP_TO_SEQ_SPI_CS2.
                DeviceDescriptor {
                    mux_index: 1,
                    cs: PinSet { port: gpio_api::Port::A, pin_mask: 1 << 0 },
                },
                // Device 2 is the KSZ8463 switch (U401).
                // Connected on port I.
                // CS is SPI_SP_TO_MGMT_MUX_CSN.
                DeviceDescriptor {
                    mux_index: 0,
                    cs: PinSet { port: gpio_api::Port::A, pin_mask: 1 << 0 },
                },
                // Device 3 is the local flash (U557).
                // Shares port B with the sequencer.
                // CS is SP_TO_FLASH_SPI_CS.
                DeviceDescriptor {
                    mux_index: 1,
                    cs: PinSet { port: gpio_api::Port::B, pin_mask: 1 << 12 },
                },
            ],
        };
    } else if #[cfg(all(target_board = "gimlet-1", feature = "spi4"))] {
        const CONFIG: ServerConfig = ServerConfig {
            registers: device::SPI4::ptr(),
            peripheral: rcc_api::Peripheral::Spi4,
            mux_options: &[
                SpiMuxOption {
                    outputs: &[
                        // SPI4 is only muxed to one position.
                        (
                            PinSet {
                                port: gpio_api::Port::E,
                                pin_mask: (1 << 2) | (1 << 6),
                            },
                            gpio_api::Alternate::AF5,
                        ),
                    ],
                    input: (
                        PinSet {
                            port: gpio_api::Port::E,
                            pin_mask: 1 << 5,
                        },
                        gpio_api::Alternate::AF5,
                    ),
                    swap_data: false,
                },
            ],
            devices: &[
                // The only device is the RoT.
                // CS is SPI_SP_TO_ROT_CS_L.
                DeviceDescriptor {
                    mux_index: 0,
                    cs: PinSet { port: gpio_api::Port::E, pin_mask: 1 << 4 },
                },
            ],
        };
    //
    // NUCLEO 743 board
    //
    } else if #[cfg(all(target_board = "nucleo-h743zi2", feature = "spi3"))] {
        const CONFIG: ServerConfig = ServerConfig {
            registers: device::SPI3::ptr(),
            peripheral: rcc_api::Peripheral::Spi3,
            mux_options: &[
                SpiMuxOption {
                    outputs: &[
                        (
                            PinSet {
                                port: gpio_api::Port::B,
                                pin_mask: 1 << 3,
                            },
                            gpio_api::Alternate::AF6,
                        ),
                        (
                            PinSet {
                                port: gpio_api::Port::B,
                                pin_mask: 1 << 5,
                            },
                            gpio_api::Alternate::AF7,
                        ),
                    ],
                    input: (
                        PinSet {
                            port: gpio_api::Port::B,
                            pin_mask: 1 << 4,
                        },
                        gpio_api::Alternate::AF6,
                    ),
                    swap_data: false,
                },
            ],
            devices: &[
                DeviceDescriptor {
                    mux_index: 0,
                    cs: PinSet { port: gpio_api::Port::A, pin_mask: 1 << 4 },
                },
            ],
        };
    //
    // Standalone build
    //
    } else if #[cfg(feature = "standalone")] {
        // whatever - nobody gonna run it
        const CONFIG: ServerConfig = ServerConfig {
            registers: device::SPI1::ptr(),
            peripheral: rcc_api::Peripheral::Spi1,
            mux_options: &[],
            devices: &[],
        };
    } else {
        compile_error!("unsupported board-controller combination");
    }
}
