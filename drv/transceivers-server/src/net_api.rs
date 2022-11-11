//! Hardware revision-independent server code for UDP interactions
//!
//! This is in a separate module to avoid polluting `main.rs` with a bunch of
//! imports from the `transceiver_messages` crate; it simply adds more functions
//! to our existing `ServerImpl`.
use crate::ServerImpl;
use drv_sidecar_front_io::transceivers::FpgaPortMasks;
use hubpack::SerializedSize;
use task_net_api::*;
use transceiver_messages::{message::*, mgmt::MemoryRegion, Error, ModuleId};

impl ServerImpl {
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
                let (msg, data) = hubpack::deserialize(rx_data_buf).unwrap();
                let (reply, size) = match self.handle_message(
                    msg,
                    data,
                    &mut tx_data_buf[Message::MAX_SIZE..],
                ) {
                    Ok(r) => r,
                    Err(e) => (HostResponse::Error(e), 0),
                };
                let out = Message {
                    header: msg.header,
                    modules: msg.modules,
                    body: MessageBody::HostResponse(reply),
                };
                // Serialize into the front of the tx buffer
                let out_size = hubpack::serialize(tx_data_buf, &out).unwrap();

                // At this point, any supplementary data is written to
                // tx_buf[Message::MAX_SIZE..].  Let's shift it backwards based
                // on the side of the leading `Message`:
                tx_data_buf.copy_within(
                    Message::MAX_SIZE..(Message::MAX_SIZE + size),
                    out_size,
                );

                meta.size = (out_size + size) as u32;
                self.net
                    .send_packet(
                        SOCKET,
                        meta,
                        &tx_data_buf[..meta.size as usize],
                    )
                    .unwrap();
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
        out: &mut [u8],
    ) -> Result<(HostResponse, usize), Error> {
        if msg.header.version != 1 {
            return Err(Error::VersionMismatch);
        }

        // Convert from the over-the-network type to our local port mask type
        let fpga_ports: u16 = msg.modules.ports.0;
        let fpga_mask = match msg.modules.fpga_id {
            0 => FpgaPortMasks {
                left: fpga_ports,
                right: 0,
            },
            1 => FpgaPortMasks {
                left: 0,
                right: fpga_ports,
            },
            i => {
                return Err(Error::InvalidFpga(i));
            }
        };

        match msg.body {
            MessageBody::SpRequest(..)
            | MessageBody::SpResponse(..)
            | MessageBody::HostResponse(..) => {
                return Err(Error::ProtocolError);
            }
            MessageBody::HostRequest(h) => {
                self.handle_host_request(h, msg.modules, fpga_mask, data, out)
            }
        }
    }

    fn handle_host_request(
        &mut self,
        h: HostRequest,
        modules: ModuleId,
        mask: FpgaPortMasks,
        data: &[u8],
        out: &mut [u8],
    ) -> Result<(HostResponse, usize), Error> {
        match h {
            HostRequest::Reset => {
                // TODO: use a more correct error code
                self.transceivers
                    .set_reset(mask)
                    .map_err(|_e| Error::ReadFailed)?;
                userlib::hl::sleep_for(1);
                self.transceivers
                    .clear_reset(mask)
                    .map_err(|_e| Error::ReadFailed)?;
                Ok((HostResponse::Ack, 0))
            }
            HostRequest::Status => {
                use drv_sidecar_front_io::{
                    transceivers::FpgaController, Addr,
                };
                use zerocopy::{BigEndian, U16};

                let fpga = self.transceivers.fpga(FpgaController::Left);

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

                let mut count = 0;
                for port in modules.ports.to_indices() {
                    let mut status = Status::empty();
                    if (enable.get() & (1 << port)) != 0 {
                        status |= Status::ENABLED;
                    }
                    if (reset.get() & (1 << port)) != 0 {
                        status |= Status::RESET;
                    }
                    if (lpmode.get() & (1 << port)) != 0 {
                        status |= Status::LOW_POWER_MODE;
                    }
                    if (present.get() & (1 << port)) != 0 {
                        status |= Status::PRESENT;
                    }
                    if (irq.get() & (1 << port)) != 0 {
                        status |= Status::INTERRUPT;
                    }
                    out[count] = status.bits();
                    count += 1;
                }
                Ok((HostResponse::Status, count))
            }
            HostRequest::Read(mem) => {
                let out_size = mem.len() as u32 * modules.ports.0.count_ones();
                if out_size as usize > transceiver_messages::MAX_MESSAGE_SIZE {
                    return Err(Error::RequestTooLarge);
                }
                self.read(mem, modules, out)?;
                Ok((HostResponse::Read(mem), out_size as usize))
            }
            HostRequest::Write(mem) => {
                let data_size = mem.len() as u32 * modules.ports.0.count_ones();
                // TODO: check equality here and return a different error?
                if data_size as usize > data.len() {
                    return Err(Error::RequestTooLarge);
                }
                self.write(mem, modules, data)?;
                Ok((HostResponse::Write(mem), 0))
            }
            HostRequest::SetPowerMode(mode) => {
                // TODO: do we need delays in between any of these operations?
                match mode {
                    PowerMode::Off => {
                        // Power disabled, LpMode enabled (the latter doesn't
                        // make a difference, but keeps us in a known state)
                        self.transceivers
                            .clear_power_enable(mask)
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
                Ok((HostResponse::Ack, 0))
            }
            HostRequest::ManagementInterface(i) => {
                todo!()
            }
        }
    }

    fn read(
        &mut self,
        mem: MemoryRegion,
        modules: ModuleId,
        out: &mut [u8],
    ) -> Result<(), Error> {
        todo!()
    }

    fn write(
        &mut self,
        mem: MemoryRegion,
        modules: ModuleId,
        data: &[u8],
    ) -> Result<(), Error> {
        todo!()
    }
}
