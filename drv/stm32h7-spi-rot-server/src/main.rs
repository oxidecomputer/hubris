// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use idol_runtime::{ClientError, Leased, RequestError, R, W};
// use idol_runtime::{NotificationHandler, ClientError, Leased, RequestError, R, W};
// TODO: This should be in drv/oxide-spi-sp-rot or some such
use drv_spi_api::{CsState, Spi, SpiError};
use drv_spi_msg::*;
use drv_stm32xx_sys_api as sys_api;
use userlib::*;

task_slot!(SPI, spi_driver);
task_slot!(SYS, sys);

use ringbuf::*;

const SPI_TO_ROT_DEVICE: u8 = 0;

// On Gemini, the STM32H753 is in a LQFP176 package with ROT_IRQ on pin2/PE3
const ROT_IRQ: sys_api::PinSet  = sys_api::PinSet {
    port: sys_api::Port::E,
    pin_mask: 1 << 3,
};

// const PACKET_MASK: u32 = 1 << 0;

#[derive(Copy, Clone, PartialEq, Debug)]
enum Trace {
    Init,
    BadMessageLength(usize),
    _BadMessageType(drv_spi_msg::MsgType),
    BadProtocol(u8),
    RspLen(usize),
    SpiError(SpiError),
    Data(u8, u8, u8, u8),
    GpioPort(u16, u16),
    Line,
    EchoMsgType,
    SprocketsMsgType,
    StatusMsgType,
    ReturnOk(MsgType, u32),
    Waited(u64, u64, u64, u64, u64),
    RotIrqTimeout,
    RotIrqAsserted,
    SendRecv(MsgType, usize, usize),
}
ringbuf!(Trace, 32, Trace::Init);

#[export_name = "main"]
fn main() -> ! {
    ringbuf_entry!(Trace::Line);
    let spi = Spi::from(SPI.get_task_id()).device(SPI_TO_ROT_DEVICE);
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.gpio_configure_input(ROT_IRQ, sys_api::Pull::None).unwrap_lite();

    let mut buffer = [0; idl::INCOMING_SIZE];
    // The larger of the two buffers.
    let message = &mut [0u8; drv_spi_msg::SPI_RSP_BUF_SIZE];
    let mut server = ServerImpl {
        sys,
        spi,
        message: *message,
    };

    loop {
        ringbuf_entry!(Trace::Line);
        idol_runtime::dispatch(&mut buffer, &mut server);
        ringbuf_entry!(Trace::Line);
    }
}

fn do_send_recv(server: &mut ServerImpl) -> Result<usize, MsgError> {
    // The server struct contains a message to be transmitted to the RoT.
    // The same buffer will hold the response from the RoT.
    ringbuf_entry!(Trace::Data(
        server.message[0],
        server.message[1],
        server.message[2],
        server.message[3]
    ));
    let msg = Msg::parse(&mut server.message[..]).unwrap_lite();
    if !msg.is_supported_version() {
        return Err(MsgError::BadMessageType);
    }
    ringbuf_entry!(Trace::Line);
    match msg.msgtype() {
        MsgType::Echo => ringbuf_entry!(Trace::EchoMsgType),
        MsgType::Sprockets => ringbuf_entry!(Trace::SprocketsMsgType),
        MsgType::Status => ringbuf_entry!(Trace::StatusMsgType),
        _ => {
            ringbuf_entry!(Trace::Line);
            return Err(MsgError::BadMessageType);
        }
    }
    let xmit_len = SPI_HEADER_SIZE + msg.payload_len();
    ringbuf_entry!(Trace::Line);

    let port = server.sys.gpio_read_input(ROT_IRQ.port).unwrap_lite();
    let rot_irq = port & ROT_IRQ.pin_mask;
    // let rot_irq = server.sys.gpio_read(ROT_IRQ).unwrap_lite();
    if 0 == rot_irq {   // We are surprised that ROT_IRQ is asserted
        // It is intended to be ok to ignore RoT responses.
        //
        // TODO: fully explore the implications of that in the context of
        // our fully developed implementation.
        //
        // The RoT must be able to observe SP resets.
        // During the normal start-up seqeunce, the RoT is controlling the
        // SP's boot up sequence. However, the SP can reset itself and
        // individual Hubris tasks may fail and be restarted.
        // TODO: Should this task send an explicit message telling the RoT
        // that the SP has fully initialize? The first Sprockets message may
        // serve as that indication.
        //
        // If the RoT ever does asynchronous transmission of messages
        // then we'll see ROT_IRQ asserted without having first sent a message.
        //
        // If SP and RoT are out of sync, e.g. this task restarts and an old
        // response is still in the RoT's transmit FIFO, then we can also see
        // ROT_IRQ asserted when not expected.

        ringbuf_entry!(Trace::GpioPort(port, ROT_IRQ.pin_mask));
        // XXX For now, async and unhandled responses are not expected.
        panic!();  
    }
    if let Err(spi_error) = server.spi.write(&server.message[0..xmit_len]) {
        // XXX this does not return
        ringbuf_entry!(Trace::SpiError(spi_error));
        return Err(MsgError::SpiServerError);
    }

    // We sleep and poll for what should be long enough for the RoT
    // to queue a response.
    // XXX For better performance and power efficiency,
    // take an interrupt on ROT_IRQ falling edge with timeout.
    // Sprockets::get_measurements took 133.4ms on Gemini
    // let mut limit = 1250000_usize; // 1.04ms Time limit is somewhat arbitrary.
    // const LIMIT_START: usize = 25000000; // 1.99ms @ sleep_for(1);
    // const LIMIT_START: usize = 1_000_000_000; // 1.73ms @ sleep_for(1)
    // const LIMIT_START: usize = 1_000_000_000; // 2.02ms @ sleep_for(2)
    // const LIMIT_START: usize = 1_000_000_000; // 1.59ms @ sleep_for(10)
    const LIMIT_START: u64 = 1_000_000_000;
    const NAP: u64 = 1;
    let mut limit = LIMIT_START; // 1.04ms Time limit is somewhat arbitrary.
                                  // TODO: put timelimit in Status
    let start: u64 = sys_get_timer().now;
    let mut stats: [u64; 4] = [0; 4]; // zero, NAP/4, NAP, >NAP
    loop {                  // Use interrupt instead of polling.
        if 0 == server.sys.gpio_read(ROT_IRQ).unwrap_lite() {
            ringbuf_entry!(Trace::RotIrqAsserted);
            break;
        }
        limit -= 1;
        if limit == 0 {
            ringbuf_entry!(Trace::RotIrqTimeout);
            break;
        }
        let pre = sys_get_timer().now;
        hl::sleep_for(NAP);
        let delta = sys_get_timer().now - pre;
        if delta == 0 {
            stats[0] += 1;
        } else if delta <= NAP/4 {
            stats[1] += 1;
        } else if delta <= NAP {
            stats[2] += 1;
        } else {
            stats[3] += 1;
        }
    }
    ringbuf_entry!(
        Trace::Waited(sys_get_timer().now - start,
          stats[0], stats[1], stats[2], stats[3]));

    /*
    // Unmask our interrupt.

    // STM32 EXTI allows for 16 interrupts for GPIOs.
    // Each of those can represent Pin X from a GPIO bank (A through K)
    // So, only one bank's Pin 3, for example, can have the #3 interrupt.
    // For ROT_IRQ, we would configure for the falling edge to trigger
    // the interrupt. That configuration should be specified in the app.toml
    // for the board. Work needs to be done to generalize the EXTI facility.
    // But, hacking in one interrupt as an example should be ok to start things
    // off.

    sys_irq_control(self.interrupt, true);
    // And wait for it to arrive.
    let _rm =
        sys_recv_closed(&mut [], self.interrupt, TaskId::KERNEL)
            .unwrap_lite();
    */

    // Read just the header.
    // Keep CSn asserted over the two reads.
    server
        .spi
        .lock(CsState::Asserted)
        .map_err(|_| MsgError::SpiServerError)?;
    ringbuf_entry!(Trace::Line);
    if let Err(spi_error) =
        server.spi.read(&mut server.message[0..SPI_HEADER_SIZE])
    {
        ringbuf_entry!(Trace::SpiError(spi_error));
        server.spi.release().unwrap_lite();
        return Err(MsgError::SpiServerError); // XXX don't hide this information
    }

    ringbuf_entry!(Trace::Data(
        server.message[0],
        server.message[1],
        server.message[2],
        server.message[3]
    ));

    let msg = Msg::parse(&mut server.message[..]).unwrap_lite();
    if !msg.is_supported_version() {
        ringbuf_entry!(Trace::BadProtocol(server.message[0]));
        server.spi.release().unwrap_lite();
        return Err(MsgError::UnsupportedProtocol);
    }
    let rlen = msg.payload_len();
    ringbuf_entry!(Trace::RspLen(rlen));
    if rlen > server.message.len() - SPI_HEADER_SIZE {
        ringbuf_entry!(Trace::BadMessageLength(rlen));
        server.spi.release().unwrap_lite();
        return Err(MsgError::BadTransferSize);
    }
    if let Err(spi_error) = server
        .spi
        .read(&mut server.message[SPI_HEADER_SIZE..SPI_HEADER_SIZE + rlen])
    {
        ringbuf_entry!(Trace::SpiError(spi_error));
        server.spi.release().unwrap_lite();
        return Err(MsgError::SpiServerError);
    }
    server.spi.release().unwrap_lite();

    let msg = Msg::parse(&mut server.message[0..rlen + SPI_HEADER_SIZE])
        .unwrap_lite();
    match msg.payload_get() {
        Err(err) => Err(err),
        Ok(buf) => Ok(buf.len()),
    }
}

struct ServerImpl {
    sys: sys_api::Sys,
    spi: drv_spi_api::SpiDevice,
    pub message: [u8; SPI_RSP_BUF_SIZE],
}

impl idl::InOrderSpiMsgImpl for ServerImpl {
    /// A client sends a message for SPDM processing.
    fn send_recv(
        &mut self,
        _: &RecvMessage,
        msgtype: drv_spi_msg::MsgType,
        source: Leased<R, [u8]>,
        sink: Leased<W, [u8]>,
    ) -> Result<[u32; 2], RequestError<MsgError>> {
        ringbuf_entry!(Trace::Line);
        let mut msg =
            drv_spi_msg::Msg::parse(&mut self.message[..]).unwrap_lite();

        msg.set_version();
        msg.set_len(source.len());
        msg.set_msgtype(msgtype);
        ringbuf_entry!(Trace::SendRecv(msgtype, source.len(), msg.payload_buf().len()));
        if source.len() > msg.payload_buf().len() {
            ringbuf_entry!(Trace::Line);
        }
        // Read the message into our local buffer offset by the header size
        source
            .read_range(
                0..source.len(),
                &mut msg.payload_buf()[0..source.len()],
            )
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        ringbuf_entry!(Trace::Line);

        // Send message, then receive response using the same local buffer.
        do_send_recv(&mut *self)?;
        ringbuf_entry!(Trace::Line);

        let msg = drv_spi_msg::Msg::parse(&mut self.message[..]).unwrap_lite();
        sink.write_range(0..msg.payload_len(), msg.payload_get().unwrap_lite())
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        ringbuf_entry!(Trace::ReturnOk(
            msg.msgtype(),
            msg.payload_len() as u32
        ));
        // TODO: I'd like to return a tuple of MsgType and rlen.
        Ok([msg.msgtype() as u8 as u32, msg.payload_len() as u32])
    }
}

//impl NotificationHandler for ServerImpl<'_> {
//    fn current_notification_mask(&self) -> u32 {
//        // RoT will notify via GPIO/ROT_IRQ signal.
//        // ROT_IRQ
//    }

// fn handle_notification(&mut self, _bits: u32) {
// XXX complete this
// if bits & ROT_IRQ != 0 {
// self.spi.read_response()
// userlib::sys_irq_control(ROT_IRQ, true);
// }
// }
//}

mod idl {
    use super::{MsgError, MsgType};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
