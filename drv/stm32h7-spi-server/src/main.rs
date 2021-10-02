//! Server task for the STM32H7 SPI peripheral.
//!
//! Currently this hardcodes the clock rate and doesn't manage chip select.
//!
//! # IPC Protocol
//!
//! ## Exchange (1)
//!
//! Transmits data on MOSI and simultaneously receives data on MISO.
//!
//! Transmitted data is read from a byte buffer passed as borrow 0. This borrow
//! must be readable.
//!
//! Received data is either written into borrow 0 (overwriting transmitted
//! data), or can be written into a separate buffer by passing it as borrow 1.
//! Whichever borrow is used for received data must be writable, and if it's
//! separate from the transmit buffer, the two buffers must be the same length.

#![no_std]
#![no_main]

use drv_spi_api::*;
use ringbuf::*;
use stm32h7::stm32h743 as device;
use userlib::*;

use drv_stm32h7_gpio_api as gpio_api;
use drv_stm32h7_rcc_api as rcc_api;
use drv_stm32h7_spi as spi_core;

declare_task!(RCC, rcc_driver);
declare_task!(GPIO, gpio_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Start(Operation, (usize, usize)),
    Tx(usize, u8),
    Rx(usize, u8),
    None,
}

ringbuf!(Trace, 64, Trace::None);

const IRQ_MASK: u32 = 1;

#[export_name = "main"]
fn main() -> ! {
    let rcc_driver = rcc_api::Rcc::from(get_task_id(RCC));

    cfg_if::cfg_if! {
        if #[cfg(feature = "spi1")] {
            compile_error!("spi1 not supported on this board");
        } else if #[cfg(feature = "spi2")] {
            #[cfg(any(
                feature = "spi3",
                feature = "spi4",
                feature = "spi5",
                feature = "spi6"
            ))]
            compile_error!("can only set one peripheral");

            let peripheral = rcc_api::Peripheral::Spi2;
            let registers = unsafe { &*device::SPI2::ptr() };

            cfg_if::cfg_if! {
                if #[cfg(target_board = "gemini-bu-1")] {
                    let pins = [(
                        gpio_api::Port::I,
                        (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3),
                        gpio_api::Alternate::AF5,
                    )];
                } else if #[cfg(target_board = "gimlet-1")] {
                    //
                    // On Gimlet, spi2 is used for three different devices:
                    // the management network (KSZ8463 at refdes U401),
                    // the local flash (U557), and the sequencer (U476).
                    // This is across two different ports (port B and port I)
                    // -- and because there is more than one device, we
                    // explicitly do not include CS (PI0, PB12) in each;
                    // these will need to be explicitly managed by the caller
                    // to select the appropriate chip.
                    //
                    let pins = [(
                        gpio_api::Port::I,
                        (1 << 1) | (1 << 2) | (1 << 3),
                        gpio_api::Alternate::AF5,
                    ), (
                        gpio_api::Port::B,
                        (1 << 13) | (1 << 14) | (1 << 15),
                        gpio_api::Alternate::AF5,
                    )];
                } else {
                    compile_error!("spi2 not supported on this board");
                }
            }
        } else if #[cfg(feature = "spi3")] {
            #[cfg(any(feature = "spi4", feature = "spi5", feature = "spi6"))]
            compile_error!("can only set one peripheral");

            let peripheral = rcc_api::Peripheral::Spi3;
            let registers = unsafe { &*device::SPI3::ptr() };

            cfg_if::cfg_if! {
                if #[cfg(target_board = "gimletlet-2")] {
                    let pins = [(
                        gpio_api::Port::C,
                        (1 << 10) | (1 << 11) | (1 << 12),
                        gpio_api::Alternate::AF6,
                    ), (
                        gpio_api::Port::A,
                        1 << 15,
                        gpio_api::Alternate::AF6,
                    )];
                } else if #[cfg(target_board = "nucleo-h743zi2")] {
                    let pins = [(
                        gpio_api::Port::A,
                        1 << 4,
                        gpio_api::Alternate::AF6,
                    ), (
                        gpio_api::Port::B,
                        (1 << 3) | (1 << 4),
                        gpio_api::Alternate::AF6,
                    ), (
                        gpio_api::Port::B,
                        1 << 5,
                        gpio_api::Alternate::AF7,
                    )];
                } else {
                    compile_error!("spi3 not supported on this board");
                }
            }
        } else if #[cfg(feature = "spi4")] {
            #[cfg(any(feature = "spi5", feature = "spi6"))]
            compile_error!("can only set one peripheral");

            let peripheral = rcc_api::Peripheral::Spi4;
            let registers = unsafe { &*device::SPI4::ptr() };

            cfg_if::cfg_if! {
                if #[cfg(target_board = "gemini-bu-1")] {
                    //
                    // On Gemini, the main connection to the RoT:
                    //  PE2 = SCK
                    //  PE4 = CS
                    //  PE5 = MISO
                    //  PE6 = MOSI
                    //
                    // If you need debugging, configure these pins:
                    //  PE12 = SCK
                    //  PE11 = CS
                    //  PE13 = MISO
                    //  PE14 = MOSI
                    //
                    // Make sure MISO and MOSI are connected to something when
                    // debugging, otherwise you may get unexpected output.
                    //
                    let pins = [(
                        gpio_api::Port::E,
                        (1 << 2) | (1 << 4) | (1 << 5) | (1 << 6),
                        gpio_api::Alternate::AF5,
                    )];
                } else if #[cfg(target_board = "gimletlet-2")] {
                    let pins = [(
                        gpio_api::Port::E,
                        (1 << 11) | (1 << 12) | (1 << 13) | (1 << 14),
                        gpio_api::Alternate::AF5,
                    )];
                } else if #[cfg(target_board = "gimlet-1")] {
                    //
                    // On Gimlet -- as with Gemini -- the main connection to
                    // the RoT is on the PE pins.
                    //
                    let pins = [(
                        gpio_api::Port::E,
                        (1 << 2) | (1 << 4) | (1 << 5) | (1 << 6),
                        gpio_api::Alternate::AF5,
                    )];
                } else {
                    compile_error!("spi4 not supported on this board");
                }
            }
        } else if #[cfg(feature = "spi5")] {
            compile_error!("spi5 not supported on this board");
        } else if #[cfg(feature = "spi6")] {
            cfg_if::cfg_if! {
                if #[cfg(target_board = "gimletlet-2")] {
                    let pins = [(
                        gpio_api::Port::G,
                        (1 << 8) | (1 << 12) | (1 << 13) | (1 << 14),
                        gpio_api::Alternate::AF5,
                    )]
                } else {
                    compile_error!("spi6 not supported on this board");
                }
            }
        } else if #[cfg(feature = "standalone")] {
            let peripheral = rcc_api::Peripheral::Spi2;
            let registers = unsafe { &*device::SPI2::ptr() };
            let pins = [( gpio_api::Port::A, 0, gpio_api::Alternate::AF0 )];
        } else {
            compile_error!(
                "must enable one of: spi1, spi2, spi3, spi4, spi5, spi6"
            );
        }
    }

    rcc_driver.enable_clock(peripheral);
    rcc_driver.leave_reset(peripheral);
    let mut spi = spi_core::Spi::from(registers);

    // This should correspond to '0' in the standard SPI parlance
    spi.initialize(
        device::spi1::cfg1::MBR_A::DIV256,
        8,
        device::spi1::cfg2::COMM_A::FULLDUPLEX,
        device::spi1::cfg2::LSBFRST_A::MSBFIRST,
        device::spi1::cfg2::CPHA_A::FIRSTEDGE,
        device::spi1::cfg2::CPOL_A::IDLELOW,
        device::spi1::cfg2::SSOM_A::ASSERTED,
    );

    let gpio_driver = gpio_api::Gpio::from(get_task_id(GPIO));

    for pin in &pins {
        gpio_driver
            .configure(
                pin.0,
                pin.1,
                gpio_api::Mode::Alternate,
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::High,
                gpio_api::Pull::None,
                pin.2,
            )
            .unwrap();
    }

    loop {
        hl::recv_without_notification(
            // Our only operations use zero-length messages, so we can use a
            // zero-length buffer here.
            &mut [],
            |op, msg| match op {
                // Yes, this case matches all the enum values right now. This is
                // insurance if we were to add a fourth!
                Operation::Exchange | Operation::Read | Operation::Write => {
                    // We can take varying numbers of leases, so we'll do lease
                    // verification ourselves just below.
                    let lease_count = msg.lease_count();
                    let ((), caller) = msg.fixed().ok_or(SpiError::BadArg)?;

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
                                Operation::Exchange => {
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
                            // receiving. The transmist buffer cannot be larger
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

                    // As we're done with the borrows, we can now resume the
                    // caller.
                    caller.reply(());

                    Ok(())
                }
            },
        );
    }
}
