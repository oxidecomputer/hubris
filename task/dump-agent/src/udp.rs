// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{ServerImpl, DUMP_READ_SIZE};
use hubpack::SerializedSize;
use ringbuf::*;
use task_net_api::{
    LargePayloadBehavior, RecvError, SendError, SocketName, UdpMetadata,
};
use userlib::*;

static_assertions::const_assert_eq!(
    DUMP_READ_SIZE,
    humpty::udp::DUMP_READ_SIZE
);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    GotHeader(humpty::udp::Header),
    DeserializeError(hubpack::Error),
    DeserializeHeaderError(hubpack::Error),
    SendError(SendError),
    WrongVersion(u8),
}

ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////
// Constants and static assertions
const SOCKET: SocketName = SocketName::dump_agent;

const SOCKET_TX_SIZE: usize = task_net_api::SOCKET_TX_SIZE[SOCKET as usize];
const SOCKET_RX_SIZE: usize = task_net_api::SOCKET_RX_SIZE[SOCKET as usize];

// Check our buffer sizes against packet sizes in the TOML file
static_assertions::const_assert!(
    humpty::udp::ResponseMessage::MAX_SIZE <= SOCKET_TX_SIZE
);
static_assertions::const_assert!(
    humpty::udp::RequestMessage::MAX_SIZE <= SOCKET_RX_SIZE
);

// The response message is a tuple (Header, Result<Response, Error>)
static_assertions::const_assert_eq!(
    humpty::udp::ResponseMessage::MAX_SIZE,
    humpty::udp::Header::MAX_SIZE
        + <Result<humpty::udp::Response, humpty::udp::Error>>::MAX_SIZE
);

// The request message is a tuple (Header, Request)
static_assertions::const_assert_eq!(
    humpty::udp::RequestMessage::MAX_SIZE,
    humpty::udp::Header::MAX_SIZE + humpty::udp::Request::MAX_SIZE
);

/// Grabs references to the static descriptor/buffer receive rings. Can only be
/// called once.
///
/// Note that we're using the packet sizes configured in the TOML file, not the
/// smallest possible packet sizes. This gives us a little wiggle room: if a
/// future message protocol version increases max packet size, we'll be able to
/// at least read it and decode the header.
pub fn claim_statics() -> (
    &'static mut [u8; SOCKET_RX_SIZE],
    &'static mut [u8; SOCKET_TX_SIZE],
) {
    use static_cell::ClaimOnceCell;
    static BUFS: ClaimOnceCell<([u8; SOCKET_RX_SIZE], [u8; SOCKET_TX_SIZE])> =
        ClaimOnceCell::new(([0; SOCKET_RX_SIZE], [0; SOCKET_TX_SIZE]));
    let (rx, tx) = BUFS.claim();
    (rx, tx)
}

////////////////////////////////////////////////////////////////////////////////

impl ServerImpl {
    pub fn check_net(
        &mut self,
        rx_data_buf: &mut [u8],
        tx_data_buf: &mut [u8],
    ) {
        match self.net.recv_packet(
            SOCKET,
            LargePayloadBehavior::Discard,
            rx_data_buf,
        ) {
            Ok(meta) => self.handle_packet(meta, rx_data_buf, tx_data_buf),
            Err(RecvError::QueueEmpty | RecvError::ServerRestarted) => {
                // Our incoming queue is empty or `net` restarted. Wait for more
                // packets in dispatch, back in the main loop.
            }
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
                Ok((mut header, msg)) => {
                    ringbuf_entry!(Trace::GotHeader(header));
                    let response = self.handle_message(header, msg);
                    header.version = humpty::udp::version::CURRENT;
                    let reply =
                        humpty::udp::ResponseMessage { header, response };
                    Some(hubpack::serialize(tx_data_buf, &reply).unwrap())
                }
                Err(e) => {
                    // We couldn't deserialize the header; give up
                    ringbuf_entry!(Trace::DeserializeHeaderError(e));
                    None
                }
            };

        if let Some(out_len) = out_len {
            meta.size = out_len as u32;
            if let Err(e) = self.net.send_packet(
                SOCKET,
                meta,
                &tx_data_buf[..meta.size as usize],
            ) {
                ringbuf_entry!(Trace::SendError(e));
                match e {
                    // We'll drop packets if the outgoing queue is full or the
                    // server has died; the host is responsible for retrying.
                    SendError::QueueFull | SendError::ServerRestarted => (),
                }
            }
        }
    }

    /// Handles a single message
    fn handle_message(
        &mut self,
        header: humpty::udp::Header,
        data: &[u8],
    ) -> Result<humpty::udp::Response, humpty::udp::Error> {
        // If the header is < our min version, then we can't deserialize at all,
        // so return an error immediately.
        if header.version < humpty::udp::version::MIN {
            ringbuf_entry!(Trace::WrongVersion(header.version));
            return Err(humpty::udp::Error::VersionMismatch {
                ours: humpty::udp::version::CURRENT,
                theirs: header.version,
            });
        }

        use humpty::udp::{Request, Response};
        let r = match hubpack::deserialize::<Request>(data) {
            Ok((msg, _data)) => match msg {
                Request::ReadDump { index, offset } => {
                    let mut out = [0u8; 256];
                    self.read_dump(index, offset, out.as_mut_slice())?;
                    Response::ReadDump(out)
                }
                Request::InitializeDump => {
                    self.initialize().map(|()| Response::InitializeDump)?
                }
                Request::AddDumpSegment { address, length } => self
                    .add_dump_segment(address, length)
                    .map(|()| Response::AddDumpSegment)?,
                Request::TakeDump => {
                    self.take_dump().map(|()| Response::TakeDump)?
                }
                Request::DumpTask { task_index } => {
                    self.dump_task(task_index).map(Response::DumpTask)?
                }
                Request::DumpTaskRegion {
                    task_index,
                    start,
                    length,
                } => self
                    .dump_task_region(task_index, start, length)
                    .map(Response::DumpTaskRegion)?,
                Request::ReinitializeDumpFrom { index } => self
                    .reinitialize_dump_from(index)
                    .map(|()| Response::ReinitializeDumpFrom)?,
                Request::GetImageId => {
                    Response::GetImageId(kipc::read_image_id().to_le_bytes())
                }
            },
            Err(e) => {
                // This message is from a newer version, so it makes sense that
                // we failed to deserialize it.
                if header.version > humpty::udp::version::CURRENT {
                    ringbuf_entry!(Trace::WrongVersion(header.version));
                    return Err(humpty::udp::Error::VersionMismatch {
                        ours: humpty::udp::version::CURRENT,
                        theirs: header.version,
                    });
                } else {
                    ringbuf_entry!(Trace::DeserializeError(e));
                    return Err(humpty::udp::Error::DeserializeError);
                }
            }
        };
        Ok(r)
    }
}
