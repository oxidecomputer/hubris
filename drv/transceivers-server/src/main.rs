// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_i2c_devices::pca9956b::Error;
use drv_sidecar_front_io::{
    leds::FullErrorSummary,
    leds::Leds,
    transceivers::{FpgaPortMasks, Transceivers},
};
use drv_sidecar_seq_api::{SeqError, Sequencer};
use drv_transceivers_api::{
    ModulesStatus, TransceiversError, NUM_PORTS, PAGE_SIZE_BYTES,
};
use hubpack::SerializedSize;
use idol_runtime::{
    ClientError, Leased, NotificationHandler, RequestError, R, W,
};
use ringbuf::*;
use task_net_api::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(SEQ, seq);
task_slot!(NET, net);

// Both incoming and outgoing messages use the Message type, so we use it to
// size our Tx / Rx buffers.
const MAX_UDP_MESSAGE_SIZE: usize =
    transceiver_messages::message::Message::MAX_SIZE;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq, Eq)]
enum Trace {
    None,
    FrontIOReady(bool),
    FrontIOSeqErr(SeqError),
    LEDInit,
    LEDInitComplete,
    LEDInitError(Error),
    LEDErrorSummary(FullErrorSummary),
    LEDUninitialized,
    LEDUpdateError(Error),
    ModulePresenceUpdate(u32),
    TransceiversError(TransceiversError),
}
ringbuf!(Trace, 16, Trace::None);

struct ServerImpl {
    transceivers: Transceivers,
    leds: Leds,
    net: Net,
    modules_present: u32,
    led_error: FullErrorSummary,
    leds_initialized: bool,
}

const NET_NOTIFICATION_MASK: u32 = 1 << 0; // Matches configuration in app.toml
const TIMER_NOTIFICATION_MASK: u32 = 1 << 1;
const TIMER_INTERVAL: u64 = 500;

// Errors are being suppressed here due to a miswiring of the I2C bus at the
// LED controller parts. They will not be accessible without rework to older
// hardware, and newer (correct) hardware will be replacing the hold stuff
// very soon.
//
// TODO: remove conditional compilation path once sidecar-a is sunset
#[cfg(target_board = "sidecar-a")]
impl ServerImpl {
    fn led_init(&mut self) {
        let _ = self.leds.initialize_current();
        let _ = self.leds.turn_on_system_led();
        self.leds_initialized = true;
        ringbuf_entry!(Trace::LEDInitComplete);
    }

    fn led_update(&self, presence: u32) {
        let _ = self.leds.update_led_state(presence);
    }
}

#[cfg(not(target_board = "sidecar-a"))]
impl ServerImpl {
    fn led_init(&mut self) {
        match self
            .leds
            .initialize_current()
            .and(self.leds.turn_on_system_led())
        {
            Ok(_) => {
                self.leds_initialized = true;
                ringbuf_entry!(Trace::LEDInitComplete);
            }
            Err(e) => ringbuf_entry!(Trace::LEDInitError(e)),
        };
    }

    fn led_update(&self, presence: u32) {
        if self.leds_initialized {
            match self.leds.update_led_state(presence) {
                Ok(_) => (),
                Err(e) => ringbuf_entry!(Trace::LEDUpdateError(e)),
            }
        }
    }
}

// Hardware-independent server code
impl ServerImpl {
    fn check_net(&mut self, rx_data_buf: &mut [u8], tx_data_buf: &mut [u8]) {
        const SOCKET: SocketName = SocketName::transceivers;

        use transceiver_messages::message::*;
        match self.net.recv_packet(
            SOCKET,
            LargePayloadBehavior::Discard,
            rx_data_buf,
        ) {
            Ok(mut meta) => {
                let (msg, data) = hubpack::deserialize(rx_data_buf).unwrap();
                let reply = match self.handle_message(msg, data) {
                    Ok(r) => r,
                    Err(e) => HostResponse::Error(e),
                };
                let out = Message {
                    header: msg.header,
                    modules: msg.modules,
                    body: MessageBody::HostResponse(reply),
                };
                // Serialize into the tx buffer and send it out!
                let out_size = hubpack::serialize(tx_data_buf, &out).unwrap();
                meta.size = out_size as u32;
                self.net
                    .send_packet(SOCKET, meta, &tx_data_buf[0..out_size])
                    .unwrap();
            }
            Err(RecvError::QueueEmpty) => {
                // Our incoming queue is empty. Wait for more packets
                // in dispatch_n below.
            }
            Err(RecvError::NotYours | RecvError::Other) => panic!(),
        }
    }

    fn handle_message(
        &mut self,
        msg: transceiver_messages::message::Message,
        data: &[u8],
    ) -> Result<
        transceiver_messages::message::HostResponse,
        transceiver_messages::Error,
    > {
        use transceiver_messages::message::*;
        if msg.header.version != 1 {
            return Err(transceiver_messages::Error::VersionMismatch);
        }

        // Convert from the over-the-network type to our local port mask type
        let fpga_ports: u32 = msg.modules.ports.into();
        let fpga_mask = match msg.modules.fpga {
            transceiver_messages::Fpga::LEFT => FpgaPortMasks {
                left: fpga_ports as u16,
                right: 0,
            },
            transceiver_messages::Fpga::RIGHT => FpgaPortMasks {
                left: 0,
                right: fpga_ports as u16,
            },
            i => {
                return Err(
                    // TODO: Fpga -> u8 conversion
                    transceiver_messages::Error::InvalidFpga(todo!()),
                );
            }
        };

        match msg.body {
            MessageBody::SpRequest(..)
            | MessageBody::SpResponse(..)
            | MessageBody::HostResponse(..) => {
                return Err(transceiver_messages::Error::ProtocolError);
            }
            MessageBody::HostRequest(h) => {
                self.handle_host_request(h, fpga_mask, data)
            }
        }
    }

    fn handle_host_request(
        &mut self,
        h: transceiver_messages::message::HostRequest,
        mask: FpgaPortMasks,
        data: &[u8],
    ) -> Result<
        transceiver_messages::message::HostResponse,
        transceiver_messages::Error,
    > {
        use transceiver_messages::message::*;
        match h {
            HostRequest::Reset => {
                // TODO: use a more correct error code
                self.transceivers
                    .set_reset(mask)
                    .map_err(|_e| transceiver_messages::Error::ReadFailed)?;
                userlib::hl::sleep_for(1);
                self.transceivers
                    .clear_reset(mask)
                    .map_err(|_e| transceiver_messages::Error::ReadFailed)?;
                Ok(HostResponse::Reset)
            }
            HostRequest::Status => {
                todo!()
            }
            HostRequest::Read(mem) => {
                todo!()
            }
            HostRequest::Write(mem) => {
                todo!()
            }
        }
    }
}
////////////////////////////////////////////////////////////////////////////////

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
        TIMER_NOTIFICATION_MASK | NET_NOTIFICATION_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if (bits & NET_NOTIFICATION_MASK) != 0 {
            // Nothing to do here; we'll handle it in the main loop
        }

        if (bits & TIMER_NOTIFICATION_MASK) != 0 {
            // Check for errors
            if self.leds_initialized {
                let errors = self.leds.error_summary().unwrap();
                if errors != self.led_error {
                    self.led_error = errors;
                    ringbuf_entry!(Trace::LEDErrorSummary(errors));
                }
            } else {
                ringbuf_entry!(Trace::LEDUninitialized);
            }

            // Query module presence and update LEDs accordingly
            let presence = match self.transceivers.get_modules_status() {
                Ok(status) => status.present,
                Err(_) => 0,
            };

            if presence != self.modules_present {
                self.led_update(presence);

                self.modules_present = presence;
                ringbuf_entry!(Trace::ModulePresenceUpdate(presence));
            }

            let next_deadline = sys_get_timer().now + TIMER_INTERVAL;
            sys_set_timer(Some(next_deadline), TIMER_NOTIFICATION_MASK)
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    loop {
        // This is a temporary workaround that makes sure the FPGAs are up
        // before we start doing things with them. A more sophisticated
        // notification system will be put in place.
        let seq = Sequencer::from(SEQ.get_task_id());
        loop {
            let ready = seq.front_io_phy_ready();
            match ready {
                Ok(true) => {
                    ringbuf_entry!(Trace::FrontIOReady(true));
                    break;
                }
                Err(SeqError::NoFrontIOBoard) => {
                    ringbuf_entry!(Trace::FrontIOSeqErr(
                        SeqError::NoFrontIOBoard
                    ));
                    break;
                }
                _ => {
                    ringbuf_entry!(Trace::FrontIOReady(false));
                    userlib::hl::sleep_for(10)
                }
            }
        }

        let transceivers = Transceivers::new(FRONT_IO.get_task_id());
        let leds = Leds::new(
            &i2c_config::devices::pca9956b_front_leds_left(I2C.get_task_id()),
            &i2c_config::devices::pca9956b_front_leds_right(I2C.get_task_id()),
        );

        let net = Net::from(NET.get_task_id());
        let (tx_data_buf, rx_data_buf) = claim_statics();
        let mut server = ServerImpl {
            transceivers,
            leds,
            net,
            modules_present: 0,
            led_error: Default::default(),
            leds_initialized: false,
        };

        ringbuf_entry!(Trace::LEDInit);

        server.transceivers.enable_led_controllers().unwrap();
        server.led_init();

        // This will put our timer in the past, immediately forcing an update
        let deadline = sys_get_timer().now;
        sys_set_timer(Some(deadline), TIMER_NOTIFICATION_MASK);

        let mut buffer = [0; idl::INCOMING_SIZE];
        loop {
            server.check_net(
                tx_data_buf.as_mut_slice(),
                rx_data_buf.as_mut_slice(),
            );
            idol_runtime::dispatch_n(&mut buffer, &mut server);
        }
    }
}
////////////////////////////////////////////////////////////////////////////////

/// Grabs references to the static descriptor/buffer receive rings. Can only be
/// called once.
pub fn claim_statics() -> (
    &'static mut [u8; MAX_UDP_MESSAGE_SIZE],
    &'static mut [u8; MAX_UDP_MESSAGE_SIZE],
) {
    const S: usize = MAX_UDP_MESSAGE_SIZE;
    mutable_statics::mutable_statics! {
        static mut TX_BUF: [u8; S] = [|| 0u8; _];
        static mut RX_BUF: [u8; S] = [|| 0u8; _];
    }
}
////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::{ModulesStatus, TransceiversError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
