//! A driver for the rp2040 GPIO

#![no_std]
#![no_main]

use core::convert::Infallible;

pub use drv_rp2040_gpio_api::{Direction, Gpio, Value};
use idol_runtime::NotificationHandler;
use rp2040_hal::pac;
use userlib::RecvMessage;

type IdolRes<T> = Result<T, idol_runtime::RequestError<Infallible>>;
type UnitRes = IdolRes<()>;

struct ServerImpl {
    sio: pac::SIO,
    io_bank: pac::IO_BANK0,
}

impl ServerImpl {
    fn new() -> Self {
        Self {
            // SAFETY: This task should be the only task HANDLING SIO
            sio: unsafe { pac::SIO::steal() },
            // SAFETY: This task should be the only task handling IO_BANK0
            io_bank: unsafe { pac::IO_BANK0::steal() },
        }
    }
}

impl idl::InOrderPinsImpl for ServerImpl {
    fn set_dir(
        &mut self,
        _: &RecvMessage,
        pin: Gpio,
        dir: Direction,
    ) -> UnitRes {
        // it isn't clear if you need to do this for `In` but I'm going to just to be safe
        self.io_bank
            .gpio(pin as usize)
            .gpio_ctrl()
            .modify(|_, w| w.funcsel().sio());
        match dir {
            Direction::In => self
                .sio
                .gpio_oe_clr()
                .write(|w| unsafe { w.bits(pin.bit()) }),
            Direction::Out => self
                .sio
                .gpio_oe_set()
                .write(|w| unsafe { w.bits(pin.bit()) }),
        };
        Ok(())
    }

    fn set_pin(&mut self, _msg: &RecvMessage, pin: Gpio) -> UnitRes {
        self.sio
            .gpio_out_set()
            .write(|w| unsafe { w.bits(pin.bit()) });
        Ok(())
    }

    fn clear_pin(&mut self, _msg: &RecvMessage, pin: Gpio) -> UnitRes {
        self.sio
            .gpio_out_clr()
            .write(|w| unsafe { w.bits(pin.bit()) });
        Ok(())
    }

    fn toggle_pin(&mut self, _msg: &RecvMessage, pin: Gpio) -> UnitRes {
        self.sio
            .gpio_out_xor()
            .write(|w| unsafe { w.bits(pin.bit()) });
        Ok(())
    }

    fn read_pin(&mut self, _msg: &RecvMessage, pin: Gpio) -> IdolRes<Value> {
        if self.sio.gpio_in().read().bits() & pin.bit() == pin.bit() {
            Ok(Value::High)
        } else {
            Ok(Value::Low)
        }
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    let mut incoming = [0; idl::INCOMING_SIZE];

    let mut server = ServerImpl::new();

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

mod idl {
    pub use drv_rp2040_gpio_api::{Direction, Gpio, Value};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
