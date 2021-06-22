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

use stm32h7::stm32h743 as device;
use userlib::*;

use drv_stm32h7_gpio_api as gpio_api;
use drv_stm32h7_rcc_api as rcc_api;
use drv_stm32h7_spi as spi_core;

#[cfg(feature = "standalone")]
const RCC: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;

#[cfg(feature = "standalone")]
const GPIO: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const GPIO: Task = Task::gpio_driver;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq)]
enum Operation {
    Read = 0b01,
    Write = 0b10,
    Exchange = 0b11,
}

impl Operation {
    pub fn is_read(self) -> bool {
        self as u32 & 1 != 0
    }

    pub fn is_write(self) -> bool {
        self as u32 & 0b10 != 0
    }
}

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
}

// TODO: it is super unfortunate to have to write this by hand, but deriving
// ToPrimitive makes us check at runtime whether the value fits
impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

const IRQ_MASK: u32 = 1;

#[export_name = "main"]
fn main() -> ! {
    let rcc_driver = rcc_api::Rcc::from(get_task_id(RCC));

    // SPI4 is the connection from SP -> RoT
    rcc_driver.enable_clock(rcc_api::Peripheral::Spi4);
    rcc_driver.leave_reset(rcc_api::Peripheral::Spi4);

    // Manufacture a pointer to SPI4 because the stm32h7 crate won't help us
    // Safety: we're dereferencing a pointer to a guaranteed-valid address of
    // registers.
    let registers = unsafe { &*device::SPI4::ptr() };

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

    // The main connection to RoT
    // PE2 = SCK
    // PE4 = CS
    // PE5 = MISO
    // PE6 = MOSI
    //
    // If you need debugging, the following pins can be configured
    // PE12 = SCK
    // PE11 = CS
    // PE13 = MISO
    // PE14 = MOSI
    //
    // Make sure MISO and MOSI are connected to something when debugging,
    // otherwise you may get unexpected output.
    gpio_driver
        .configure(
            gpio_api::Port::E,
            (1 << 2) | (1 << 4) | (1 << 5) | (1 << 6),
            gpio_api::Mode::Alternate,
            gpio_api::OutputType::PushPull,
            gpio_api::Speed::High,
            gpio_api::Pull::None,
            gpio_api::Alternate::AF5,
        )
        .unwrap();

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
                    let ((), caller) =
                        msg.fixed().ok_or(ResponseCode::BadArg)?;

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
                                borrow.info().ok_or(ResponseCode::BadArg)?;

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
                                return Err(ResponseCode::BadArg);
                            }

                            let read_borrow = if op.is_write() {
                                Some(borrow.clone())
                            } else {
                                None
                            };
                            let write_borrow =
                                if op.is_read() { Some(borrow) } else { None };

                            (read_borrow, write_borrow, info.len)
                        }
                        2 if op == Operation::Exchange => {
                            // Caller has provided two leases, the first as a
                            // data source and the second as a data sink. This
                            // is only legal if we are both transmitting and
                            // receiving. The buffers are currently required to
                            // be the same length for simplicity, though this
                            // restriction is not inherent and could be lifted
                            // with some effort.
                            let src_borrow = caller.borrow(0);
                            let src_info = src_borrow
                                .info()
                                .ok_or(ResponseCode::BadArg)?;

                            if !src_info
                                .attributes
                                .contains(LeaseAttributes::READ)
                            {
                                return Err(ResponseCode::BadArg);
                            }

                            let dst_borrow = caller.borrow(1);
                            let dst_info = dst_borrow
                                .info()
                                .ok_or(ResponseCode::BadArg)?;

                            if !dst_info
                                .attributes
                                .contains(LeaseAttributes::WRITE)
                            {
                                return Err(ResponseCode::BadArg);
                            }

                            if dst_info.len != src_info.len {
                                return Err(ResponseCode::BadArg);
                            }

                            (Some(src_borrow), Some(dst_borrow), src_info.len)
                        }
                        _ => return Err(ResponseCode::BadArg),
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
                    if xfer_len == 0 || xfer_len >= 0x1_0000 {
                        return Err(ResponseCode::BadArg);
                    }

                    // We have a reasonable-looking request containing (a)
                    // reasonable-looking lease(s). This is our commit point.

                    // Make sure SPI is on.
                    spi.enable(xfer_len as u16);
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
                                // Transfer byte from caller to TX FIFO.
                                let byte: u8 = tx_data
                                    .read_at(*tx_pos)
                                    .ok_or(ResponseCode::BadArg)?;
                                spi.send8(byte);
                                *tx_pos += 1;

                                // If we have _just_ finished...
                                if *tx_pos == xfer_len {
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
                                    .ok_or(ResponseCode::BadArg)?;
                                *rx_pos += 1;

                                if *rx_pos == xfer_len {
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
