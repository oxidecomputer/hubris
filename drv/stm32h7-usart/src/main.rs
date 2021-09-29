//! A driver for the STM32H7 U(S)ART.
//!
//! # IPC protocol
//!
//! ## `write` (1)
//!
//! Sends the contents of lease #0. Returns when completed.

#![no_std]
#![no_main]

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;
#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

use userlib::*;

declare_task!(RCC, rcc_driver);
declare_task!(GPIO, gpio_driver);

#[derive(Copy, Clone, Debug, FromPrimitive)]
enum Operation {
    Write = 1,
}

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
    Busy = 3,
}

// TODO: it is super unfortunate to have to write this by hand, but deriving
// ToPrimitive makes us check at runtime whether the value fits
impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

struct Transmit {
    caller: hl::Caller<()>,
    len: usize,
    pos: usize,
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_usart();

    // From thin air, pluck a pointer to the USART register block.
    //
    // Safety: this is needlessly unsafe in the API. The USART is essentially a
    // static, and we access it through a & reference so aliasing is not a
    // concern. Were it literally a static, we could just reference it.
    #[cfg(feature = "h7b3")]
    let usart = unsafe { &*device::USART1::ptr() };
    #[cfg(feature = "h743")]
    let usart = unsafe { &*device::USART3::ptr() };

    // The UART has clock and is out of reset, but isn't actually on until we:
    usart.cr1.write(|w| w.ue().enabled());
    // Work out our baud rate divisor.
    // TODO: this module should _not_ know our clock rate. That's a hack.
    #[cfg(feature = "h7b3")]
    const CLOCK_HZ: u32 = 280_000_000;
    #[cfg(feature = "h743")]
    const CLOCK_HZ: u32 = 200_000_000;

    const BAUDRATE: u32 = 115_200;
    const CYCLES_PER_BIT: u32 = (CLOCK_HZ + (BAUDRATE / 2)) / BAUDRATE;
    usart.brr.write(|w| w.brr().bits(CYCLES_PER_BIT as u16));

    // Enable the UART and transmitter.
    usart.cr1.modify(|_, w| w.ue().enabled().te().enabled());

    configure_pins();

    // Turn on our interrupt. We haven't enabled any interrupt sources at the
    // USART side yet, so this won't trigger notifications yet.
    sys_irq_control(1, true);

    // Field messages.
    let mask = 1;
    let mut tx: Option<Transmit> = None;

    loop {
        hl::recv(
            // Buffer (none required)
            &mut [],
            // Notification mask
            mask,
            // State to pass through to whichever closure below gets run
            &mut tx,
            // Notification handler
            |txref, bits| {
                if bits & 1 != 0 {
                    // Handling an interrupt. To allow for spurious interrupts,
                    // check the individual conditions we care about, and
                    // unconditionally re-enable the IRQ at the end of the handler.

                    if usart.isr.read().txe().bit() {
                        // TX register empty. Do we need to send something?
                        step_transmit(&usart, txref);
                    }

                    sys_irq_control(1, true);
                }
            },
            // Message handler
            |txref, op, msg| match op {
                Operation::Write => {
                    // Validate lease count and buffer sizes first.
                    let ((), caller) =
                        msg.fixed_with_leases(1).ok_or(ResponseCode::BadArg)?;

                    // Deny incoming writes if we're already running one.
                    if txref.is_some() {
                        return Err(ResponseCode::Busy);
                    }

                    let borrow = caller.borrow(0);
                    let info = borrow.info().ok_or(ResponseCode::BadArg)?;
                    // Provide feedback to callers if they fail to provide a
                    // readable lease (otherwise we'd fail accessing the borrow
                    // later, which is a defection case and we won't reply at
                    // all).
                    if !info.attributes.contains(LeaseAttributes::READ) {
                        return Err(ResponseCode::BadArg);
                    }

                    // Okay! Begin a transfer!
                    *txref = Some(Transmit {
                        caller,
                        pos: 0,
                        len: info.len,
                    });

                    // OR the TX register empty signal into the USART interrupt.
                    usart.cr1.modify(|_, w| w.txeie().enabled());

                    // We'll do the rest as interrupts arrive.
                    Ok(())
                }
            },
        );
    }
}

fn turn_on_usart() {
    use drv_stm32h7_rcc_api::{Peripheral, Rcc};
    let rcc_driver = Rcc::from(get_task_id(RCC));

    #[cfg(feature = "h7b3")]
    const PORT: Peripheral = Peripheral::Usart1;

    #[cfg(feature = "h743")]
    const PORT: Peripheral = Peripheral::Usart3;

    rcc_driver.enable_clock(PORT);
    rcc_driver.leave_reset(PORT);
}

fn configure_pins() {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver = get_task_id(GPIO);
    let gpio_driver = Gpio::from(gpio_driver);

    #[cfg(feature = "h7b3")]
    const TX_RX_MASK: (Port, u16) = (Port::A, (1 << 9) | (1 << 10));
    #[cfg(feature = "h743")]
    const TX_RX_MASK: (Port, u16) = (Port::D, (1 << 8) | (1 << 9));

    gpio_driver
        .configure(
            TX_RX_MASK.0,
            TX_RX_MASK.1,
            Mode::Alternate,
            OutputType::PushPull,
            Speed::High,
            Pull::None,
            Alternate::AF7,
        )
        .unwrap();
}

fn step_transmit(
    usart: &device::usart1::RegisterBlock,
    tx: &mut Option<Transmit>,
) {
    // Clearer than just using replace:
    fn end_transmission(
        usart: &device::usart1::RegisterBlock,
        state: &mut Option<Transmit>,
    ) -> hl::Caller<()> {
        usart.cr1.modify(|_, w| w.txeie().disabled());
        core::mem::replace(state, None).unwrap().caller
    }

    let txs = if let Some(txs) = tx { txs } else { return };

    if let Some(byte) = txs.caller.borrow(0).read_at::<u8>(txs.pos) {
        // Stuff byte into transmitter.
        usart
            .tdr
            .write(|w| unsafe { w.tdr().bits(u16::from(byte)) });

        txs.pos += 1;
        if txs.pos == txs.len {
            end_transmission(usart, tx).reply(());
        }
    } else {
        end_transmission(usart, tx).reply_fail(ResponseCode::BadArg);
    }
}
