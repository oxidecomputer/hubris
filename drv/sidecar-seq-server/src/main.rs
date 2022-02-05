// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Sidecar sequencing process.

#![no_std]
#![no_main]

use ringbuf::*;
use userlib::*;

use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_sidecar_seq_api::{PowerState, SeqError};
use idol_runtime::{NotificationHandler, RequestError};

task_slot!(SYS, sys);
task_slot!(I2C, i2c_driver);

mod payload;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    A2,
    GetState,
    SetState(PowerState, PowerState),
    LoadClockConfig,
    ClockConfigWrite(usize),
    ClockConfigSuccess(usize),
    ClockConfigFailed(usize, ResponseCode),
    Done,
    None,
}

ringbuf!(Trace, 64, Trace::None);

const TIMER_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 1000;

struct ServerImpl {
    state: PowerState,
    clockgen: I2cDevice,
    led: drv_stm32xx_sys_api::PinSet,
    led_on: bool,
    deadline: u64,
}

impl ServerImpl {
    fn led_init(&mut self) {
        use drv_stm32xx_sys_api::*;

        let sys = SYS.get_task_id();
        let sys = Sys::from(sys);

        // Make the LED an output.
        sys.gpio_configure_output(
            self.led,
            OutputType::PushPull,
            Speed::High,
            Pull::None,
        )
        .unwrap();
    }

    fn led_on(&mut self) {
        use drv_stm32xx_sys_api::*;

        let sys = SYS.get_task_id();
        let sys = Sys::from(sys);
        sys.gpio_set_to(self.led, true).unwrap();
        self.led_on = true;
    }

    fn led_off(&mut self) {
        use drv_stm32xx_sys_api::*;

        let sys = SYS.get_task_id();
        let sys = Sys::from(sys);
        sys.gpio_set_to(self.led, false).unwrap();
        self.led_on = false;
    }

    fn led_toggle(&mut self) {
        if self.led_on {
            self.led_off();
        } else {
            self.led_on();
        }
    }
}

impl idl::InOrderSequencerImpl for ServerImpl {
    fn get_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<PowerState, RequestError<SeqError>> {
        ringbuf_entry!(Trace::GetState);
        Ok(self.state)
    }

    fn set_state(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
    ) -> Result<(), RequestError<SeqError>> {
        ringbuf_entry!(Trace::SetState(self.state, state));

        match (self.state, state) {
            _ => Err(RequestError::Runtime(SeqError::IllegalTransition)),
        }
    }

    fn load_clock_config(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SeqError>> {
        ringbuf_entry!(Trace::LoadClockConfig);

        let mut packet = 0;

        payload::idt8a3xxxx_payload(|buf| {
            ringbuf_entry!(Trace::ClockConfigWrite(packet));
            match self.clockgen.write(buf) {
                Err(err) => {
                    ringbuf_entry!(Trace::ClockConfigFailed(packet, err));
                    Err(SeqError::ClockConfigFailed)
                }

                Ok(_) => {
                    ringbuf_entry!(Trace::ClockConfigSuccess(packet));
                    packet += 1;
                    Ok(())
                }
            }
        })?;

        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        self.deadline += TIMER_INTERVAL;
        self.led_toggle();
        sys_set_timer(Some(self.deadline), TIMER_MASK);
    }
}

#[export_name = "main"]
fn main() -> ! {
    let task = I2C.get_task_id();

    ringbuf_entry!(Trace::A2);

    let mut buffer = [0; idl::INCOMING_SIZE];

    let deadline = sys_get_timer().now;

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    sys_set_timer(Some(deadline), TIMER_MASK);

    let mut server = ServerImpl {
        state: PowerState::A2,
        clockgen: devices::idt8a34001(task)[0],
        led: drv_stm32xx_sys_api::Port::C.pin(3),
        led_on: false,
        deadline,
    };

    server.led_init();

    loop {
        ringbuf_entry!(Trace::Done);
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{PowerState, SeqError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
