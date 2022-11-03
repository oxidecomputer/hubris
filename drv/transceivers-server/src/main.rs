// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_sidecar_front_io::{
    leds::FullErrorSummary, leds::Leds, phy_smi::PhySmi,
    transceivers::Transceivers,
};
use drv_transceivers_api::{
    ModulesStatus, TransceiversError, NUM_PORTS, PAGE_SIZE_BYTES,
};
use idol_runtime::{
    ClientError, Leased, NotificationHandler, RequestError, R, W,
};
use ringbuf::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq, Eq)]
enum Trace {
    None,
    LEDInit,
    LEDInitComplete,
    LEDErrorUpdate(FullErrorSummary),
    ModulePresenceUpdate(u32),
    TransceiversError(TransceiversError),
}
ringbuf!(Trace, 16, Trace::None);

struct ServerImpl {
    transceivers: Transceivers,
    leds: Leds,
    modules_present: u32,
    led_error: FullErrorSummary,
}

const TIMER_NOTIFICATION_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 500;

impl idl::InOrderTransceiversImpl for ServerImpl {
    fn get_modules_status(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ModulesStatus, idol_runtime::RequestError<TransceiversError>>
    {
        Ok(self
            .transceivers
            .get_modules_status()
            .map_err(TransceiversError::from)?)
    }

    fn set_power_enable(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .set_power_enable(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn clear_power_enable(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .clear_power_enable(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn set_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .set_reset(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn clear_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .clear_reset(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn set_lpmode(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .set_lpmode(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn clear_lpmode(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .clear_lpmode(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn setup_i2c_op(
        &mut self,
        _msg: &userlib::RecvMessage,
        is_read: bool,
        reg: u8,
        num_bytes: u8,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if usize::from(num_bytes) > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        self.transceivers
            .setup_i2c_op(is_read, reg, num_bytes, mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn get_i2c_read_buffer(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
        dest: Leased<W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if port >= NUM_PORTS {
            return Err(TransceiversError::InvalidPortNumber.into());
        }

        if dest.len() > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        let mut buf = [0u8; PAGE_SIZE_BYTES];

        self.transceivers
            .get_i2c_read_buffer(port, &mut buf[..dest.len()])
            .map_err(TransceiversError::from)?;

        dest.write_range(0..dest.len(), &buf[..dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        Ok(())
    }

    fn set_i2c_write_buffer(
        &mut self,
        _msg: &userlib::RecvMessage,
        data: Leased<R, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if data.len() > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        let mut buf = [0u8; PAGE_SIZE_BYTES];

        data.read_range(0..data.len(), &mut buf[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        self.transceivers
            .set_i2c_write_buffer(&buf[..data.len()])
            .map_err(TransceiversError::from)?;
        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        TIMER_NOTIFICATION_MASK
    }

    // We currently only have one notification source so we are ignoring _bits
    fn handle_notification(&mut self, _bits: u32) {
        // Check for errors
        let errors = self.leds.error_summary().unwrap();
        if errors != self.led_error {
            self.led_error = errors;
            ringbuf_entry!(Trace::LEDErrorUpdate(errors))
        }

        // Query module presence and update LEDs accordingly
        let presence = match self.transceivers.get_modules_status() {
            Ok(status) => status.present,
            Err(_) => 0,
        };

        if presence != self.modules_present {
            // Errors are being suppressed here due to a miswiring of the I2C bus at
            // the LED controller parts. They will not be accessible without rework
            // to older hardware, and newer (correct) hardware will be replacing the
            // hold stuff very soon.
            // TODO: remove conditional compilation path once sidecar-a is sunset
            cfg_if::cfg_if! {
                if #[cfg(target_board = "sidecar-a")] {
                    let _ = self.leds.update_led_state(presence);
                } else {
                    self.leds.update_led_state(presence).unwrap();
                }
            }

            self.modules_present = presence;
            ringbuf_entry!(Trace::ModulePresenceUpdate(presence));
        }

        let next_deadline = sys_get_timer().now + TIMER_INTERVAL;
        sys_set_timer(Some(next_deadline), TIMER_NOTIFICATION_MASK)
    }
}

#[export_name = "main"]
fn main() -> ! {
    loop {
        // This is a temporary workaround that makes sure the FPGAs are up
        // before we start doing things with them. A more sophisticated
        // notification system will be put in place.
        let phy_smi = PhySmi::new(FRONT_IO.get_task_id());
        while !phy_smi.phy_powered_up_and_ready().unwrap() {
            userlib::hl::sleep_for(10);
        }

        let transceivers = Transceivers::new(FRONT_IO.get_task_id());
        let leds = Leds::new(
            &i2c_config::devices::pca9956b_front_leds_left(I2C.get_task_id()),
            &i2c_config::devices::pca9956b_front_leds_right(I2C.get_task_id()),
        );

        let mut server = ServerImpl {
            transceivers,
            leds,
            modules_present: 0,
            led_error: Default::default(),
        };

        ringbuf_entry!(Trace::LEDInit);

        server.transceivers.enable_led_controllers().unwrap();

        // Errors are being suppressed here due to a miswiring of the I2C bus at
        // the LED controller parts. They will not be accessible without rework
        // to older hardware, and newer (correct) hardware will be replacing the
        // hold stuff very soon.
        // TODO: remove conditional compilation path once sidecar-a is sunset
        cfg_if::cfg_if! {
            if #[cfg(target_board = "sidecar-a")] {
                let _ = server.leds.initialize_current();
                let _ = server.leds.turn_on_system_led();
            } else {
                server.leds.initialize_current().unwrap();
                server.leds.turn_on_system_led().unwrap();
            }
        }

        ringbuf_entry!(Trace::LEDInitComplete);

        // This will put our timer in the past, immediately forcing an update
        let deadline = sys_get_timer().now;
        sys_set_timer(Some(deadline), TIMER_NOTIFICATION_MASK);

        let mut buffer = [0; idl::INCOMING_SIZE];
        loop {
            idol_runtime::dispatch_n(&mut buffer, &mut server);
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::{ModulesStatus, TransceiversError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
