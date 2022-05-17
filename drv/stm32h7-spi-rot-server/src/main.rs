// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};
// use idol_runtime::{NotificationHandler, ClientError, Leased, RequestError, R, W};
// TODO: This should be in drv/oxide-spi-sp-rot or some such
use userlib::*;
use drv_spi_api::{Spi,SpiError,CsState};
use drv_stm32xx_sys_api as sys_api;
use drv_spi_msg::*;

task_slot!(SPI, spi_driver);
task_slot!(SYS, sys);
const SPI_TO_ROT_DEVICE: u8 = 0;

use ringbuf::*;

const _ROT_IRQ: u32 = 1 << 0;   // XXX this is not plumbed yet.

// const PACKET_MASK: u32 = 1 << 0;

#[derive(Copy, Clone, PartialEq, Debug)]
enum Trace {
    Init,
    BadMessageLength(usize),
    _BadMessageType(drv_spi_msg::MsgType),
    BadProtocol(u8),
    RspLen(usize),
    SpiError(SpiError),
    Data(u8,u8,u8,u8),
    Line,
}
ringbuf!(Trace, 32, Trace::Init);

#[export_name = "main"]
fn main() -> ! {
    ringbuf_entry!(Trace::Line);
    let spi = Spi::from(SPI.get_task_id()).device(SPI_TO_ROT_DEVICE);
    let _sys = sys_api::Sys::from(SYS.get_task_id());

    let mut buffer = [0; idl::INCOMING_SIZE];
    let message = &mut [0u8; drv_spi_msg::SPI_BUFFER_SIZE];
    let mut server = ServerImpl {
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
            server.message[3]));
    let msg = Msg::parse(&mut server.message[..]).unwrap_lite();
    if !msg.is_supported_version() {
        return Err(MsgError::BadMessageType);
    }
    ringbuf_entry!(Trace::Line);
    match msg.msgtype() {
        MsgType::Echo | MsgType::Sprockets => {},
        _ => {
            ringbuf_entry!(Trace::Line);
            return Err(MsgError::BadMessageType)
        },
    }
    let xmit_len = SPI_HEADER_SIZE + msg.payload_len();
    ringbuf_entry!(Trace::Line);

    if let Err(spi_error) = server.spi.write(&server.message[0..xmit_len]) { // XXX this does not return
        ringbuf_entry!(Trace::SpiError(spi_error));
        return Err(MsgError::SpiServerError);
    }
    ringbuf_entry!(Trace::Line);

    // Right now, we sleep for what should be long enough for the RoT
    // to queue a response. In the future, we need to watch ROT_IRQ.
    hl::sleep_for(1); // XXX 1 ms is arbitrary, IRQ will remove need.
    ringbuf_entry!(Trace::Line);

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
    server.spi.lock(CsState::Asserted).map_err(|_| MsgError::SpiServerError)?;
    ringbuf_entry!(Trace::Line);
    if let Err(spi_error) =
        server.spi.read(&mut server.message[0..SPI_HEADER_SIZE]) {
            ringbuf_entry!(Trace::SpiError(spi_error));
            server.spi.release().unwrap_lite();
            return Err(MsgError::SpiServerError);  // XXX don't hide this information
    }

    ringbuf_entry!(Trace::Data(server.message[0], server.message[1], server.message[2], server.message[3]));

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
    if let Err(spi_error) = server.spi.read(&mut server.message[SPI_HEADER_SIZE..SPI_HEADER_SIZE+rlen]) {
        ringbuf_entry!(Trace::SpiError(spi_error));
        server.spi.release().unwrap_lite();
        return Err(MsgError::SpiServerError);
    }
    server.spi.release().unwrap_lite();

    let msg = Msg::parse(&mut server.message[0..rlen+SPI_HEADER_SIZE]).unwrap_lite();
    match msg.payload_get() {
        Err(err) => Err(err),
        Ok(buf) => Ok(buf.len()),
    }
}

struct ServerImpl {
    spi: drv_spi_api::SpiDevice,
    pub message: [u8; SPI_BUFFER_SIZE],
}

impl idl::InOrderSpiMsgImpl for ServerImpl {
    /// A client sends a message for SPDM processing.
    fn send_recv(
        &mut self,
        _: &RecvMessage,
        msgtype: drv_spi_msg::MsgType,
        source: LenLimit<Leased<R, [u8]>, MAX_SPI_MSG_PAYLOAD_SIZE>,
        sink: LenLimit<Leased<W, [u8]>, MAX_SPI_MSG_PAYLOAD_SIZE>,
    ) -> Result<[u32; 2], RequestError<MsgError>> {
        ringbuf_entry!(Trace::Line);
        let mut msg = drv_spi_msg::Msg::parse(&mut self.message[..]).unwrap_lite();

        msg.set_version();
        msg.set_len(source.len());
        msg.set_msgtype(msgtype);
        // Read the message into our local buffer offset by the header size
        ringbuf_entry!(Trace::Line);
        source.read_range(0..source.len(), &mut msg.payload_buf()[0..source.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        ringbuf_entry!(Trace::Line);

        // Send message, then receive response using the same local buffer.
        do_send_recv(&mut *self)?;
        ringbuf_entry!(Trace::Line);

        let msg = drv_spi_msg::Msg::parse(&mut self.message[..]).unwrap_lite();
        sink.write_range(0..msg.payload_len(), msg.payload_get().unwrap_lite())
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        ringbuf_entry!(Trace::Line);
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
    use super::{MsgType,MsgError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
