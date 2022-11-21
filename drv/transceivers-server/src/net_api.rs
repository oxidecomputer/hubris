// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hardware revision-independent server code for UDP interactions
//!
//! This is in a separate module to avoid polluting `main.rs` with a bunch of
//! imports from the `transceiver_messages` crate; it simply adds more functions
//! to our existing `ServerImpl`.
use crate::ServerImpl;
use drv_sidecar_front_io::{
    transceivers::{FpgaController, FpgaPortMasks, Transceivers},
    Addr, Reg,
};
use hubpack::SerializedSize;
use ringbuf::*;
use task_net_api::*;
use transceiver_messages::{
    message::*,
    mgmt::{MemoryRead, MemoryWrite, Page},
    Error, ModuleId,
};

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    DeserializeError(hubpack::Error),
    DeserializeHeaderError(hubpack::Error),
    SendError(SendError),
    Reset(ModuleId),
    Status(ModuleId),
    Read(ModuleId, MemoryRead),
    Write(ModuleId, MemoryWrite),
    UnexpectedHostResponse(HostResponse),
    GotSpRequest,
    GotSpResponse,
    WrongVersion(u8),
}

ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

/// Convert from the over-the-network type to our local FPGA type
fn get_fpga(m: ModuleId) -> Result<FpgaController, Error> {
    match m.fpga_id {
        0 => Ok(FpgaController::Left),
        1 => Ok(FpgaController::Right),
        i => Err(Error::InvalidFpga(i)),
    }
}

/// Convert from the over-the-network type to our local port mask type
fn get_mask(m: ModuleId) -> Result<FpgaPortMasks, Error> {
    let fpga_ports: u16 = m.ports.0;
    match m.fpga_id {
        0 => Ok(FpgaPortMasks {
            left: fpga_ports,
            right: 0,
        }),
        1 => Ok(FpgaPortMasks {
            left: 0,
            right: fpga_ports,
        }),
        i => Err(Error::InvalidFpga(i)),
    }
}

////////////////////////////////////////////////////////////////////////////////

impl ServerImpl {
    /// Attempt to read and handle data from the `net` socket
    pub fn check_net(
        &mut self,
        rx_data_buf: &mut [u8],
        tx_data_buf: &mut [u8],
    ) {
        const SOCKET: SocketName = SocketName::transceivers;

        match self.net.recv_packet(
            SOCKET,
            LargePayloadBehavior::Discard,
            rx_data_buf,
        ) {
            Ok(mut meta) => {
                // Modify meta.size based on the output packet size
                let out_len = match hubpack::deserialize(rx_data_buf) {
                    Ok((msg, data)) => {
                        self.handle_message(msg, data, tx_data_buf)
                    }
                    Err(e) => {
                        // At this point, deserialization has failed, so we
                        // can't handle the packet.  We'll attempt to
                        // deserialize *just the header* (which should never
                        // chnage), in the hopes of logging a more detailed
                        // error message about a version mismatch.
                        ringbuf_entry!(Trace::DeserializeError(e));
                        match hubpack::deserialize::<Header>(rx_data_buf) {
                            Ok((header, _)) => {
                                ringbuf_entry!(Trace::WrongVersion(
                                    header.version
                                ));
                            }
                            Err(e) => {
                                ringbuf_entry!(Trace::DeserializeHeaderError(
                                    e
                                ));
                            }
                        }
                        None
                    }
                };

                if let Some(out_len) = out_len {
                    meta.size = out_len;
                    if let Err(e) = self.net.send_packet(
                        SOCKET,
                        meta,
                        &tx_data_buf[..meta.size as usize],
                    ) {
                        // We'll drop packets if the outgoing queue is full;
                        // the host is responsible for retrying.
                        //
                        // Other errors are unexpected and panic.
                        ringbuf_entry!(Trace::SendError(e));
                        match e {
                            SendError::QueueFull => (),
                            SendError::Other
                            | SendError::NotYours
                            | SendError::InvalidVLan => panic!(),
                        }
                    }
                }
            }
            Err(RecvError::QueueEmpty) => {
                // Our incoming queue is empty. Wait for more packets
                // in dispatch_n, back in the main loop.
            }
            Err(RecvError::NotYours | RecvError::Other) => panic!(),
        }
    }

    /// Handles a single message from the host with supplementary data in `data`
    ///
    /// Returns a response and a `usize` indicating how much was written to the
    /// `out` buffer.
    fn handle_message(
        &mut self,
        msg: Message,
        data: &[u8],
        tx_data_buf: &mut [u8],
    ) -> Option<u32> {
        // If the version is mismatched, then we can't trust the deserialization
        // (even though it nominally succeeded); don't reply.
        if msg.header.version != 1 {
            ringbuf_entry!(Trace::WrongVersion(msg.header.version));
            return None;
        }

        let (reply, data_len) = match msg.body {
            // These messages should never be sent to us, and we reply
            // with a `ProtocolError` below.
            MessageBody::SpRequest(..) => {
                ringbuf_entry!(Trace::GotSpRequest);
                (SpResponse::Error(Error::ProtocolError), 0)
            }
            MessageBody::SpResponse(..) => {
                ringbuf_entry!(Trace::GotSpResponse);
                (SpResponse::Error(Error::ProtocolError), 0)
            }
            // Nothing implemented yet
            MessageBody::HostResponse(r) => {
                ringbuf_entry!(Trace::UnexpectedHostResponse(r));
                return None;
            }
            // Happy path: the host is asking something of us!
            MessageBody::HostRequest(h) => {
                match self.handle_host_request(
                    h,
                    msg.modules,
                    data,
                    &mut tx_data_buf[Message::MAX_SIZE..],
                ) {
                    Ok(r) => r,
                    Err(e) => (SpResponse::Error(e), 0),
                }
            }
        };

        // Serialize the Message into the front of the tx buffer
        let out = Message {
            header: msg.header,
            modules: msg.modules,
            body: MessageBody::SpResponse(reply),
        };
        let msg_len = hubpack::serialize(tx_data_buf, &out).unwrap();

        // At this point, any supplementary data is written to
        // `tx_data_buf[Message::MAX_SIZE..]`.  Let's shift it backwards based
        // on the size of the leading `Message`:
        tx_data_buf.copy_within(
            Message::MAX_SIZE..(Message::MAX_SIZE + data_len),
            msg_len,
        );
        Some((msg_len + data_len) as u32)
    }

    fn handle_host_request(
        &mut self,
        h: HostRequest,
        modules: ModuleId,
        data: &[u8],
        out: &mut [u8],
    ) -> Result<(SpResponse, usize), Error> {
        match h {
            HostRequest::Reset => {
                ringbuf_entry!(Trace::Reset(modules));
                let mask = get_mask(modules)?;

                // TODO: use a more correct error code
                self.transceivers
                    .set_reset(mask)
                    .map_err(|_e| Error::ReadFailed)?;
                userlib::hl::sleep_for(1);
                self.transceivers
                    .clear_reset(mask)
                    .map_err(|_e| Error::ReadFailed)?;
                Ok((SpResponse::Ack, 0))
            }
            HostRequest::Status => {
                use zerocopy::{BigEndian, U16};

                ringbuf_entry!(Trace::Status(modules));
                let fpga = get_fpga(modules)?;
                let fpga = self.transceivers.fpga(fpga);

                // This is a bit awkward: the FPGA will get _every_ module's
                // status (for the given FPGA), then we'll unpack to only the
                // ones that we care about
                let enable: U16<BigEndian> = fpga
                    .read(Addr::QSFP_CTRL_EN_H)
                    .map_err(|_e| Error::ReadFailed)?;
                let reset: U16<BigEndian> = fpga
                    .read(Addr::QSFP_CTRL_RESET_H)
                    .map_err(|_e| Error::ReadFailed)?;
                let lpmode: U16<BigEndian> = fpga
                    .read(Addr::QSFP_CTRL_LPMODE_H)
                    .map_err(|_e| Error::ReadFailed)?;
                let present: U16<BigEndian> = fpga
                    .read(Addr::QSFP_STATUS_PRESENT_H)
                    .map_err(|_e| Error::ReadFailed)?;
                let irq: U16<BigEndian> = fpga
                    .read(Addr::QSFP_STATUS_IRQ_H)
                    .map_err(|_e| Error::ReadFailed)?;

                // Write one bitfield per active port in the ModuleId
                let mut count = 0;
                for mask in modules.ports.to_indices().map(|i| 1 << i) {
                    let mut status = Status::empty();
                    if (enable.get() & mask) != 0 {
                        status |= Status::ENABLED;
                    }
                    if (reset.get() & mask) != 0 {
                        status |= Status::RESET;
                    }
                    if (lpmode.get() & mask) != 0 {
                        status |= Status::LOW_POWER_MODE;
                    }
                    if (present.get() & mask) != 0 {
                        status |= Status::PRESENT;
                    }
                    if (irq.get() & mask) != 0 {
                        status |= Status::INTERRUPT;
                    }
                    // Convert from Status -> u8 and write to the output buffer
                    out[count] = status.bits();
                    count += 1;
                }
                Ok((SpResponse::Status, count))
            }
            HostRequest::Read(mem) => {
                ringbuf_entry!(Trace::Read(modules, mem));
                let out_size = mem.len() as u32 * modules.ports.0.count_ones();
                if out_size as usize > transceiver_messages::MAX_PAYLOAD_SIZE {
                    return Err(Error::RequestTooLarge);
                }
                self.read(mem, modules, out)?;
                Ok((SpResponse::Read(mem), out_size as usize))
            }
            HostRequest::Write(mem) => {
                ringbuf_entry!(Trace::Write(modules, mem));
                let data_size = mem.len() as u32 * modules.ports.0.count_ones();
                // TODO: check equality here and return a different error?
                if data_size as usize > data.len() {
                    return Err(Error::RequestTooLarge);
                }
                self.write(mem, modules, data)?;
                Ok((SpResponse::Write(mem), 0))
            }
            HostRequest::SetPowerMode(mode) => {
                let mask = get_mask(modules)?;
                // TODO: do we need delays in between any of these operations?
                match mode {
                    PowerMode::Off => {
                        // Power disabled, LpMode enabled (the latter doesn't
                        // make a difference, but keeps us in a known state)
                        self.transceivers
                            .clear_power_enable(mask)
                            .map_err(|_e| Error::ReadFailed)?;
                        self.transceivers
                            .set_reset(mask)
                            .map_err(|_e| Error::ReadFailed)?;
                        self.transceivers
                            .set_lpmode(mask)
                            .map_err(|_e| Error::ReadFailed)?;
                    }
                    PowerMode::Low => {
                        // Power enabled, LpMode enabled
                        self.transceivers
                            .set_lpmode(mask)
                            .map_err(|_e| Error::ReadFailed)?;
                        self.transceivers
                            .set_power_enable(mask)
                            .map_err(|_e| Error::ReadFailed)?;
                        self.transceivers
                            .clear_reset(mask)
                            .map_err(|_e| Error::ReadFailed)?;
                    }
                    PowerMode::High => {
                        // Power enabled, LpMode disabled
                        self.transceivers
                            .clear_lpmode(mask)
                            .map_err(|_e| Error::ReadFailed)?;
                        self.transceivers
                            .set_power_enable(mask)
                            .map_err(|_e| Error::ReadFailed)?;
                    }
                }
                Ok((SpResponse::Ack, 0))
            }
            HostRequest::ManagementInterface(i) => {
                todo!()
            }
        }
    }

    fn select_page(
        &mut self,
        page: Page,
        mask: FpgaPortMasks,
    ) -> Result<(), Error> {
        // Common to both CMIS and SFF-8636
        const BANK_SELECT: u8 = 0x7E;
        const PAGE_SELECT: u8 = 0x7F;

        // We can always write the lower page; upper pages require modifying
        // registers in the transceiver to select it.
        if let Some(page) = page.page() {
            self.transceivers
                .set_i2c_write_buffer(&[page])
                .map_err(|_e| Error::WriteFailed)?;
            self.transceivers
                .setup_i2c_write(PAGE_SELECT, 1, mask)
                .map_err(|_e| Error::WriteFailed)?;
            self.wait_and_check_i2c(mask)?;
        }

        if let Some(bank) = page.bank() {
            self.transceivers
                .set_i2c_write_buffer(&[bank])
                .map_err(|_e| Error::ReadFailed)?;
            self.transceivers
                .setup_i2c_write(BANK_SELECT, 1, mask)
                .map_err(|_e| Error::ReadFailed)?;
            self.wait_and_check_i2c(mask)?;
        }
        Ok(())
    }

    fn wait_and_check_i2c(&mut self, mask: FpgaPortMasks) -> Result<(), Error> {
        // TODO: use a better error type here
        let err_mask = self
            .transceivers
            .wait_and_check_i2c(mask)
            .map_err(|_e| Error::WriteFailed)?;
        if err_mask.left != 0 || err_mask.right != 0 {
            // FPGA reported an I2C error
            return Err(Error::WriteFailed);
        }
        Ok(())
    }

    fn read(
        &mut self,
        mem: MemoryRead,
        modules: ModuleId,
        out: &mut [u8],
    ) -> Result<(), Error> {
        let controller = get_fpga(modules)?;
        let mask = get_mask(modules)?;

        // Switch pages (if necessary)
        self.select_page(*mem.page(), mask)?;

        // Ask the FPGA to start the read
        self.transceivers
            .setup_i2c_read(mem.offset(), mem.len(), mask)
            .map_err(|_e| Error::ReadFailed)?;

        let fpga = self.transceivers.fpga(controller);

        for (port, out) in modules
            .ports
            .to_indices()
            .zip(out.chunks_mut(mem.len() as usize))
        {
            // The status register is contiguous with the output buffer, so
            // we'll read them all in a single pass.  This should normally
            // terminate with a single read, since I2C is faster than Hubris
            // IPC.
            let mut buf = [0u8; 129];
            loop {
                fpga.read_bytes(
                    Transceivers::read_status_address(port),
                    &mut buf[0..(out.len() + 1)],
                )
                .map_err(|_e| Error::ReadFailed)?;
                let status = buf[0];

                // Use QSFP::PORT0 for constants, since they're all identical
                if status & Reg::QSFP::PORT0_I2C_STATUS::BUSY == 0 {
                    // Check error mask
                    if status & Reg::QSFP::PORT0_I2C_STATUS::ERROR != 0 {
                        return Err(Error::ReadFailed);
                    } else {
                        out.copy_from_slice(&buf[1..][..out.len()]);
                        break;
                    }
                }
                userlib::hl::sleep_for(1);
            }
        }
        Ok(())
    }

    fn write(
        &mut self,
        mem: MemoryWrite,
        modules: ModuleId,
        data: &[u8],
    ) -> Result<(), Error> {
        let mask = get_mask(modules)?;

        self.select_page(*mem.page(), mask)
            .map_err(|_e| Error::WriteFailed)?;

        // Copy data into the FPGA write buffer
        self.transceivers
            .set_i2c_write_buffer(&data[..mem.len() as usize])
            .map_err(|_e| Error::WriteFailed)?;

        // Trigger a multicast write to all transceivers in the mask
        self.transceivers
            .setup_i2c_write(mem.offset(), mem.len(), mask)
            .map_err(|_e| Error::WriteFailed)?;
        self.wait_and_check_i2c(mask)?;

        Ok(())
    }
}
