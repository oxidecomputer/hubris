// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hardware revision-independent server code for UDP interactions
//!
//! This is in a separate module to avoid polluting `main.rs` with a bunch of
//! imports from the `transceiver_messages` crate; it simply adds more functions
//! to our existing `ServerImpl`.
//!
//! All of the API types in `transceiver_messages` operate on **physical**
//! ports, i.e. an FPGA paired by a physical port index (or mask).
use crate::ServerImpl;
use drv_sidecar_front_io::{
    transceivers::{
        FpgaController, FpgaPortMasks, PhysicalPort, PhysicalPortMask,
        PortLocation,
    },
    Addr, Reg,
};
use hubpack::SerializedSize;
use ringbuf::*;
use task_net_api::*;
use transceiver_messages::{
    message::*,
    mgmt::{ManagementInterface, MemoryRead, MemoryWrite, Page},
    Error, HwError, MacAddrs, ModuleId, PortMask,
};
use zerocopy::{LittleEndian, U16};

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    DeserializeError(hubpack::Error),
    DeserializeHeaderError(hubpack::Error),
    SendError(SendError),
    AssertReset(ModuleId),
    DeassertReset(ModuleId),
    AssertLpMode(ModuleId),
    DeassertLpMode(ModuleId),
    EnablePower(ModuleId),
    DisablePower(ModuleId),
    Status(ModuleId),
    Read(ModuleId, MemoryRead),
    Write(ModuleId, MemoryWrite),
    ManagementInterface(ManagementInterface),
    UnexpectedHostResponse(HostResponse),
    GotSpRequest,
    GotSpResponse,
    WrongVersion(u8),
    MacAddrs,
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
            left: PhysicalPortMask(fpga_ports),
            right: PhysicalPortMask::default(),
        }),
        1 => Ok(FpgaPortMasks {
            left: PhysicalPortMask::default(),
            right: PhysicalPortMask(fpga_ports),
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
        match self.net.recv_packet(
            SocketName::transceivers,
            LargePayloadBehavior::Discard,
            rx_data_buf,
        ) {
            Ok(meta) => self.handle_packet(meta, rx_data_buf, tx_data_buf),
            Err(RecvError::QueueEmpty | RecvError::ServerRestarted) => {
                // Our incoming queue is empty or `net` restarted. Wait for more
                // packets in dispatch_n, back in the main loop.
            }
            Err(RecvError::NotYours | RecvError::Other) => panic!(),
        }
    }

    fn handle_packet(
        &mut self,
        mut meta: UdpMetadata,
        rx_data_buf: &[u8],
        tx_data_buf: &mut [u8],
    ) {
        let out_len =
            match hubpack::deserialize(&rx_data_buf[0..meta.size as usize]) {
                Ok((msg, data)) => self.handle_message(msg, data, tx_data_buf),
                Err(e) => {
                    ringbuf_entry!(Trace::DeserializeError(e));
                    self.handle_deserialization_error(rx_data_buf, tx_data_buf)
                }
            };

        if let Some(out_len) = out_len {
            // Modify meta.size based on the output packet size
            meta.size = out_len;
            if let Err(e) = self.net.send_packet(
                SocketName::transceivers,
                meta,
                &tx_data_buf[..meta.size as usize],
            ) {
                // We'll drop packets if the outgoing queue is full;
                // the host is responsible for retrying.
                //
                // Other errors are unexpected and panic.
                //
                // This includes ServerRestarted, because the server should only
                // restart if the watchdog times out, and the watchdog should
                // not be timing out, because we're literally replying to a
                // packet here.
                ringbuf_entry!(Trace::SendError(e));
                match e {
                    SendError::QueueFull => (),
                    SendError::Other
                    | SendError::ServerRestarted
                    | SendError::NotYours
                    | SendError::InvalidVLan => panic!(),
                }
            }
        }
    }

    /// At this point, deserialization has failed, so we can't handle the
    /// packet.  We'll attempt to deserialize *just the header* (which should
    /// never change), in the hopes of logging a more detailed error message
    /// about a version mismatch.  If the header deserializes correctly, we'll
    /// also return a `ProtocolError` message to the host (since we've got a
    /// packet number).
    fn handle_deserialization_error(
        &mut self,
        rx_data_buf: &[u8],
        tx_data_buf: &mut [u8],
    ) -> Option<u32> {
        match hubpack::deserialize::<Header>(rx_data_buf) {
            Ok((header, _)) => {
                if header.version == version::CURRENT {
                    let n = hubpack::serialize(
                        tx_data_buf,
                        &Message {
                            header,
                            modules: ModuleId::default(),
                            body: MessageBody::SpResponse(SpResponse::Error(
                                Error::ProtocolError,
                            )),
                        },
                    );
                    Some(n.unwrap() as u32)
                } else {
                    ringbuf_entry!(Trace::WrongVersion(header.version));
                    None
                }
            }
            Err(e) => {
                ringbuf_entry!(Trace::DeserializeHeaderError(e));
                None
            }
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
        if msg.header.version != version::CURRENT {
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

        // At this point, any supplementary data was written to
        // `tx_data_buf[Message::MAX_SIZE..]`, so it's not necessarily tightly
        // packed against the end of the `Message`.  Let's shift it backwards
        // based on the size of the leading `Message`:
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
            HostRequest::Status => {
                ringbuf_entry!(Trace::Status(modules));
                let count = self.get_status(modules, out)?;
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
                if mem.len() as usize != data.len() {
                    return Err(Error::WrongDataSize);
                }
                self.write(mem, modules, data)?;
                Ok((SpResponse::Write(mem), 0))
            }
            HostRequest::AssertReset => {
                ringbuf_entry!(Trace::AssertReset(modules));
                let mask = get_mask(modules)?;
                self.transceivers.assert_reset(mask).map_err(|_e| {
                    Error::WriteFailed(HwError::ResetLWriteFailed)
                })?;
                Ok((SpResponse::Ack, 0))
            }
            HostRequest::DeassertReset => {
                ringbuf_entry!(Trace::DeassertReset(modules));
                let mask = get_mask(modules)?;
                self.transceivers.deassert_reset(mask).map_err(|_e| {
                    Error::WriteFailed(HwError::ResetLWriteFailed)
                })?;
                Ok((SpResponse::Ack, 0))
            }
            HostRequest::AssertLpMode => {
                ringbuf_entry!(Trace::AssertLpMode(modules));
                let mask = get_mask(modules)?;
                self.transceivers.assert_lpmode(mask).map_err(|_e| {
                    Error::WriteFailed(HwError::LpModeWriteFailed)
                })?;
                Ok((SpResponse::Ack, 0))
            }
            HostRequest::DeassertLpMode => {
                ringbuf_entry!(Trace::DeassertLpMode(modules));
                let mask = get_mask(modules)?;
                self.transceivers.deassert_lpmode(mask).map_err(|_e| {
                    Error::WriteFailed(HwError::LpModeWriteFailed)
                })?;
                Ok((SpResponse::Ack, 0))
            }
            HostRequest::EnablePower => {
                ringbuf_entry!(Trace::EnablePower(modules));
                let mask = get_mask(modules)?;
                self.transceivers.enable_power(mask).map_err(|_e| {
                    Error::WriteFailed(HwError::PowerEnableWriteFailed)
                })?;
                Ok((SpResponse::Ack, 0))
            }
            HostRequest::DisablePower => {
                ringbuf_entry!(Trace::DisablePower(modules));
                let mask = get_mask(modules)?;
                self.transceivers.disable_power(mask).map_err(|_e| {
                    Error::WriteFailed(HwError::PowerEnableWriteFailed)
                })?;
                Ok((SpResponse::Ack, 0))
            }
            HostRequest::ManagementInterface(i) => {
                // TODO: Implement this
                ringbuf_entry!(Trace::ManagementInterface(i));
                Ok((SpResponse::Error(Error::ProtocolError), 0))
            }
            HostRequest::MacAddrs => {
                ringbuf_entry!(Trace::MacAddrs);
                let b = self.net.get_spare_mac_addresses();
                let out = MacAddrs::new(b.base_mac, b.count.get(), b.stride)?;
                Ok((SpResponse::MacAddrs(out), 0))
            }
        }
    }

    fn get_status(
        &mut self,
        modules: ModuleId,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let fpga = get_fpga(modules)?;
        let fpga = self.transceivers.fpga(fpga);

        // This is a bit awkward: the FPGA will get _every_ module's
        // status (for the given FPGA), then we'll unpack to only the
        // ones that we care about
        let enable: U16<LittleEndian> =
            fpga.read(Addr::QSFP_POWER_EN0).map_err(|_e| {
                Error::StatusFailed(HwError::PowerEnableReadFailed)
            })?;
        let resetl: U16<LittleEndian> = fpga
            .read(Addr::QSFP_MOD_RESETL0)
            .map_err(|_e| Error::StatusFailed(HwError::ResetLReadFailed))?;
        let lpmode: U16<LittleEndian> = fpga
            .read(Addr::QSFP_MOD_LPMODE0)
            .map_err(|_e| Error::StatusFailed(HwError::LpModeReadFailed))?;
        let presentl: U16<LittleEndian> = fpga
            .read(Addr::QSFP_MOD_MODPRSL0)
            .map_err(|_e| Error::StatusFailed(HwError::ModPrsLReadFailed))?;
        let irql: U16<LittleEndian> = fpga
            .read(Addr::QSFP_MOD_INTL0)
            .map_err(|_e| Error::StatusFailed(HwError::IntLReadFailed))?;
        let pg: U16<LittleEndian> = fpga
            .read(Addr::QSFP_POWER_GOOD0)
            .map_err(|_e| Error::StatusFailed(HwError::PgReadFailed))?;
        let pg_to: U16<LittleEndian> = fpga
            .read(Addr::QSFP_POWER_GOOD_TIMEOUT0)
            .map_err(|_e| Error::StatusFailed(HwError::PgTimeoutReadFailed))?;
        let pg_lost: U16<LittleEndian> = fpga
            .read(Addr::QSFP_POWER_GOOD_LOST0)
            .map_err(|_e| Error::StatusFailed(HwError::PgLostReadFailed))?;

        // Write one bitfield per active port in the ModuleId
        let mut count = 0;
        for mask in modules.ports.to_indices().map(|i| 1 << i) {
            let mut status = Status::empty();
            if (enable.get() & mask) != 0 {
                status |= Status::ENABLED;
            }
            if (!resetl.get() & mask) != 0 {
                status |= Status::RESET;
            }
            if (lpmode.get() & mask) != 0 {
                status |= Status::LOW_POWER_MODE;
            }
            if (!presentl.get() & mask) != 0 {
                status |= Status::PRESENT;
            }
            if (!irql.get() & mask) != 0 {
                status |= Status::INTERRUPT;
            }
            if (pg.get() & mask) != 0 {
                status |= Status::POWER_GOOD;
            }
            if (pg_to.get() & mask) != 0 {
                status |= Status::FAULT_POWER_TIMEOUT;
            }
            if (pg_lost.get() & mask) != 0 {
                status |= Status::FAULT_POWER_LOST;
            }
            // Convert from Status -> u8 and write to the output buffer
            out[count] = status.bits();
            count += 1;
        }
        Ok(count)
    }

    fn select_page(
        &mut self,
        page: Page,
        mask: FpgaPortMasks,
    ) -> Result<(), HwError> {
        // Common to both CMIS and SFF-8636
        const BANK_SELECT: u8 = 0x7E;
        const PAGE_SELECT: u8 = 0x7F;

        // We can always write the lower page; upper pages require modifying
        // registers in the transceiver to select it.
        if let Some(page) = page.page() {
            self.transceivers
                .set_i2c_write_buffer(&[page])
                .map_err(|_e| HwError::PageSelectWriteBufFailed)?;
            self.transceivers
                .setup_i2c_write(PAGE_SELECT, 1, mask)
                .map_err(|_e| HwError::PageSelectWriteFailed)?;
            self.wait_and_check_i2c(mask)?;
        }

        if let Some(bank) = page.bank() {
            self.transceivers
                .set_i2c_write_buffer(&[bank])
                .map_err(|_e| HwError::BankSelectWriteBufFailed)?;
            self.transceivers
                .setup_i2c_write(BANK_SELECT, 1, mask)
                .map_err(|_e| HwError::BankSelectWriteFailed)?;
            self.wait_and_check_i2c(mask)?;
        }
        Ok(())
    }

    fn wait_and_check_i2c(
        &mut self,
        mask: FpgaPortMasks,
    ) -> Result<(), HwError> {
        let err_mask = self
            .transceivers
            .wait_and_check_i2c(mask)
            .map_err(|_e| HwError::WaitFailed)?;
        if !err_mask.left.is_empty() {
            // FPGA reported an I2C error
            return Err(HwError::I2cError(PortMask(err_mask.left.get())));
        } else if !err_mask.right.is_empty() {
            return Err(HwError::I2cError(PortMask(err_mask.right.get())));
        }
        Ok(())
    }

    fn read(
        &mut self,
        mem: MemoryRead,
        modules: ModuleId,
        out: &mut [u8],
    ) -> Result<(), Error> {
        let mask = get_mask(modules)?;

        // Switch pages (if necessary)
        self.select_page(*mem.page(), mask)
            .map_err(Error::ReadFailed)?;

        // Ask the FPGA to start the read
        self.transceivers
            .setup_i2c_read(mem.offset(), mem.len(), mask)
            .map_err(|_e| Error::ReadFailed(HwError::ReadSetupFailed))?;

        let mut failure_mask: PortMask = PortMask(0);

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
            let port_loc = PortLocation {
                controller: get_fpga(modules)?,
                port: PhysicalPort(port),
            };
            loop {
                // If we have not encountered any errors, keep pulling full
                // status + buffer payloads.
                if failure_mask.selected_transceiver_count() == 0 {
                    self.transceivers
                        .get_i2c_status_and_read_buffer(
                            port_loc,
                            &mut buf[0..(out.len() + 1)],
                        )
                        .map_err(|_e| {
                            Error::ReadFailed(HwError::ReadBufFailed)
                        })?;

                    let status = buf[0];

                    // Use QSFP::PORT0 for constants, since they're all identical
                    if status & Reg::QSFP::PORT0_STATUS::BUSY == 0 {
                        // Check error mask
                        if status & Reg::QSFP::PORT0_STATUS::ERROR != 0 {
                            // Record which port the error ocurred at so we can
                            // give the host a more meaningful error.
                            failure_mask.set(port_loc.port.get())?;
                        } else {
                            // Add data to payload
                            out.copy_from_slice(&buf[1..][..out.len()]);
                        }
                        break;
                    }

                // We have encountered at least one error, meaning we will fail
                // the entire request. We will still go get the status register
                // of each port identify which ports had errors so we can tell
                // host software where the problems occurred.
                } else {
                    let status =
                        self.transceivers.get_i2c_status(port_loc).map_err(
                            |_e| Error::ReadFailed(HwError::ReadBufFailed),
                        )?;

                    if status & Reg::QSFP::PORT0_STATUS::BUSY == 0 {
                        if status & Reg::QSFP::PORT0_STATUS::ERROR != 0 {
                            failure_mask.set(port_loc.port.get())?;
                        }
                        break;
                    }
                }

                userlib::hl::sleep_for(1);
            }
        }
        if failure_mask.selected_transceiver_count() != 0 {
            return Err(Error::ReadFailed(HwError::I2cError(failure_mask)));
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
            .map_err(Error::WriteFailed)?;

        // Copy data into the FPGA write buffer
        self.transceivers
            .set_i2c_write_buffer(&data[..mem.len() as usize])
            .map_err(|_e| Error::WriteFailed(HwError::WriteBufFailed))?;

        // Trigger a multicast write to all transceivers in the mask
        self.transceivers
            .setup_i2c_write(mem.offset(), mem.len(), mask)
            .map_err(|_e| Error::WriteFailed(HwError::WriteSetupFailed))?;
        self.wait_and_check_i2c(mask).map_err(Error::WriteFailed)?;

        Ok(())
    }
}
