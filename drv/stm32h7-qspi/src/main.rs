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

use drv_spiflash_api::*;
use ringbuf::*;
use stm32h7::stm32h743 as device;
use userlib::*;

use drv_stm32h7_gpio_api as gpio_api;
use drv_stm32h7_rcc_api as rcc_api;

mod quadspi;
mod mt25q;

declare_task!(RCC, rcc_driver);
declare_task!(GPIO, gpio_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Start(Op, (usize, usize)),
    Tx(usize, u8),
    Rx(usize, u8),
    None,
}

ringbuf!(Trace, 64, Trace::None);

const IRQ_MASK: u32 = 1;

#[export_name = "main"]
fn main() -> ! {
    let rcc_driver = rcc_api::Rcc::from(get_task_id(RCC));

    let peripheral = rcc_api::Peripheral::QuadSpi;

    let registers = unsafe { &*device::QUADSPI::ptr() };
    let qspi = quadspi::Qspi::from(registers);

    cfg_if::cfg_if! {

        if #[cfg(target_board = "nucleo-h743zi2")] {
            // The CN10 connector has seven pins labeled "QSPI".
            // Use that over more obscure alternatives.

            // PB6,  QSPI_NCS
            // PB2,  QSPI_CLK
            // PD13, QSPI_IO3   // Pull up for SingleDual mode's nHOLD
            // PD12, QSPI_IO1
            // PD11, QSPI_IO0
            // PE2,  QSPI_IO2   // Pull down for SingleDual mode's nWP
            let pins = [( // IO2 or nWP
                gpio_api::Port::E,
                1 << 2,
                gpio_api::Alternate::AF9,
                // gpio_api::OutputType::PushPull,
                // gpio_api::Speed::Low,
                // gpio_api::Pull::None,
            ), (
            // CLK
            gpio_api::Port::B,
            1 << 2,
            // gpio_api::Mode::Alternate,
            // gpio_api::OutputType::PushPull,
            // gpio_api::Speed::Low,
            // gpio_api::Pull::None,
            gpio_api::Alternate::AF9,
            ), (
            // IO0, IO1, IO3 | nHOLD
            gpio_api::Port::D,
            (1 << 13) | (1 << 12) | (1 << 11),
            // gpio_api::Mode::Alternate,
            // gpio_api::OutputType::PushPull,
            // gpio_api::Speed::Low,
            // gpio_api::Pull::None,
            gpio_api::Alternate::AF9
            ), (
            // nCS
            gpio_api::Port::B,
            1 << 6,
            // gpio_api::Mode::Alternate,
            // gpio_api::OutputType::PushPull,
            // gpio_api::Speed::Low,
            // gpio_api::Pull::None,
            gpio_api::Alternate::AF10,
            )];
        } else if #[cfg(target_board = "gemini-bu-1")] {
            // PF6  QSPI_IO3
            // PF7  QSPI_IO2
            // PF8  QSPI_IO0
            // PF9  QSPI_IO1
            // PF10 QSPI_CLK
            // PB6  QSPI_nCS
            //
            // PF4 QSPI_HOST_ACCESS (not handled here)
            // PF5 nQSPI_RESET (not handled here)
            let pins = [(
                gpio_api::Port::F,
                (1 << 6) |(1 << 7) |(1 << 10),
                // gpio_api::Mode::Alternate,
                // gpio_api::OutputType::PushPull,
                // gpio_api::Speed::Low,
                // gpio_api::Pull::None,
                gpio_api::Alternate::AF9,
            ), (
            gpio_api::Port::F,
            (1 << 8) |(1 << 9),
            // gpio_api::Mode::Alternate,
            // gpio_api::OutputType::PushPull,
            // gpio_api::Speed::Low,
            // gpio_api::Pull::None,
            gpio_api::Alternate::AF10,
            ), (
            gpio_api::Port::F,
            (1 << 6),
            // gpio_api::Mode::Alternate,
            // gpio_api::OutputType::PushPull,
            // gpio_api::Speed::Low,
            // gpio_api::Pull::None,
            gpio_api::Alternate::AF10,
            )];
        } else if #[cfg(target_board = "gimlet")] {
            // PG6  QSPI_NCS
            // PF6  QSPI_IO3
            // PF7  QSPI_IO2
            // PF8  QSPI_IO0
            // PF10 QSPI_CLK
            // PF9  QSPI_IO1

            // Also need MUX control and reset
            let pins = [(
                gpio_api::Port::G,
                (1 << 6),
                // gpio_api::Mode::Alternate,
                // gpio_api::OutputType::PushPull,
                // gpio_api::Speed::Low,
                // gpio_api::Pull::None,
                gpio_api::Alternate::AF10,
            ), (
                gpio_api::Port::F,
                (1 << 6) | (1 << 7) | (1 << 10),
                // gpio_api::Mode::Alternate,
                // gpio_api::OutputType::PushPull,
                // gpio_api::Speed::Low,
                // gpio_api::Pull::None,
                gpio_api::Alternate::AF9,
            ), (
                gpio_api::Port::F,
                (1 << 8) | (1 << 9),
                // gpio_api::Mode::Alternate,
                // gpio_api::OutputType::PushPull,
                // gpio_api::Speed::Low,
                // gpio_api::Pull::None,
                gpio_api::Alternate::AF10,
            )];
        } else {
            compile_error!("target_board unknown or missing");
        }
    }

    rcc_driver.enable_clock(peripheral);
    rcc_driver.leave_reset(peripheral);
    let mut qspi = quadspi::Qspi::from(registers);

    // This should correspond to '0' in the standard SPI parlance
    //spi.initialize(
    //    device::spi1::cfg1::MBR_A::DIV256,
    //    8,
    //    device::spi1::cfg2::COMM_A::FULLDUPLEX,
    //    device::spi1::cfg2::LSBFRST_A::MSBFIRST,
    //    device::spi1::cfg2::CPHA_A::FIRSTEDGE,
    //    device::spi1::cfg2::CPOL_A::IDLELOW,
    //    device::spi1::cfg2::SSOM_A::ASSERTED,
    //);

    let gpio_driver = gpio_api::Gpio::from(get_task_id(GPIO));

    for (port, mask, af) in &pins {
        gpio_driver
            .configure(
                *port,
                *mask,
                gpio_api::Mode::Alternate,
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::High,
                gpio_api::Pull::None,
                *af,
            )
            .unwrap();
    }

    let mut buffer = [0; 9];
    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::Write | Op::Read => {
                // We can take varying numbers of leases, so we'll do lease
                // verification ourselves just below.
                let lease_count = msg.lease_count();
                // let ((), caller) = msg.fixed().ok_or(ResponseCode::BadArg)?;

                let (payload, caller) = msg
                    .fixed_with_leases::<[u8; 9], usize>(2)
                    .ok_or(ResponseCode::BadArg)?;

                let (inst, addr, dlen) = Marshal::unmarshal(payload)?;

                let cmd = &mut quadspi::CommandConfig{ ..Default::default() };
                let cmd = mt25q::api_to_h7_sfdp(inst, cmd)?;

                // TODO: Based on inst, check for address and datalength
                // needed. Check for implicit return data lenth.
                // Check that expected data transfer is accomodated by
                // buffer.
                //

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
                            borrow.info().ok_or(ResponseCode::BadLeaseArg)?;

                        // Note that the attributes _we_ require are the
                        // inverse of the sense of the SPI operation, e.g.
                        // to read from SPI we must be able to _write_ the
                        // lease, and vice versa.
                        let required_attributes = match op {
                            Op::Read => LeaseAttributes::WRITE,
                            Op::Write => LeaseAttributes::READ,
                            //Op::Exchange => {
                            //    LeaseAttributes::WRITE | LeaseAttributes::READ
                            //}
                        };

                        if !info.attributes.contains(required_attributes) {
                            return Err(ResponseCode::BadLeaseAttributes);
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
                    /*
                    2 if op == Op::Exchange => {
                        // Caller has provided two leases, the first as a
                        // data source and the second as a data sink. This
                        // is only legal if we are both transmitting and
                        // receiving. The transmit buffer cannot be larger
                        // than the receive buffer; for any bytes for which
                        // the receive buffer exceeds the transmit buffer,
                        // a zero byte will be put on the wire.
                        let src_borrow = caller.borrow(0);
                        let src_info =
                            src_borrow.info().ok_or(ResponseCode::BadSource)?;

                        if !src_info.attributes.contains(LeaseAttributes::READ)
                        {
                            return Err(ResponseCode::BadSourceAttributes);
                        }

                        let dst_borrow = caller.borrow(1);
                        let dst_info =
                            dst_borrow.info().ok_or(ResponseCode::BadSink)?;

                        if !dst_info.attributes.contains(LeaseAttributes::WRITE)
                        {
                            return Err(ResponseCode::BadSinkAttributes);
                        }

                        if dst_info.len < src_info.len {
                            return Err(ResponseCode::ShortSinkLength);
                        }

                        (
                            Some(src_borrow),
                            Some(dst_borrow),
                            (dst_info.len, src_info.len),
                        )
                    }
                    */
                    _ => return Err(ResponseCode::BadLeaseCount),
                };

                // XXX xfer_len may not match dlen

                // That routine should have returned at least one borrow if
                // the instruction includes a data transfer.
                // Here's an assert that takes fewer text bytes than assert.
                if dlen.is_some() && data_src.is_none() && data_dst.is_none() {
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
                if (dlen.is_some() && xfer_len.0 == 0) || xfer_len.0 >= 0x1_0000
                {
                    return Err(ResponseCode::BadTransferSize);
                }

                // We have a reasonable-looking request containing (a)
                // reasonable-looking lease(s). This is our commit point.
                ringbuf_entry!(Trace::Start(op, xfer_len));

                // Make sure QSPI is on.
                qspi.enable(cmd, addr, dlen); // rename to start
                                              // Load transfer count and start the state machine. At this
                                              // point we _have_ to move the specified number of bytes
                                              // through (or explicitly cancel, but we don't).
                qspi.start(cmd); // XXX redundant with enable

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
                qspi.enable_transfer_interrupts();

                qspi.clear_eot();
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
                        if qspi.can_tx_frame() {
                            // If our position is less than our tx len,
                            // transfer a byte from caller to TX FIFO --
                            // otherwise put a dummy byte on the wire
                            let byte: u8 = if *tx_pos < xfer_len.1 {
                                tx_data
                                    .read_at(*tx_pos)
                                    .ok_or(ResponseCode::BadSourceByte)?
                            } else {
                                0u8
                            };

                            ringbuf_entry!(Trace::Tx(*tx_pos, byte));
                            qspi.send8(byte);
                            *tx_pos += 1;

                            // If we have _just_ finished...
                            if *tx_pos == xfer_len.0 {
                                // We will finish transmitting well before
                                // we're done receiving, so stop getting
                                // interrupt notifications for transmit
                                // space available during that time.
                                qspi.disable_can_tx_interrupt();
                                tx = None;
                            }

                            made_progress = true;
                        }
                    }

                    if let Some((rx_data, rx_pos)) = &mut rx {
                        if qspi.can_rx_byte() {
                            // Transfer byte from RX FIFO to caller.
                            let r = qspi.recv8();
                            rx_data
                                .write_at(*rx_pos, r)
                                .ok_or(ResponseCode::BadSinkByte)?;
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

                    if qspi.check_eot() {
                        qspi.clear_eot();
                        break;
                    }
                }

                // Wrap up the transfer and restore things to a reasonable
                // state.
                qspi.end();

                // As we're done with the borrows, we can now resume the
                // caller.
                caller.reply(0_usize);    // XXX Return bytes transferred

                Ok(())
            }
        });
    }
}
