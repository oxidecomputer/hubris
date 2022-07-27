// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Log, MgsMessage, UsartHandler, __RINGBUF};
use gateway_messages::{
    sp_impl::SocketAddrV6,
    sp_impl::{SerialConsolePacketizer, SpHandler},
    BulkIgnitionState, DiscoverResponse, IgnitionCommand, IgnitionState,
    ResponseError, SerialConsole, SpComponent, SpPort, SpState,
};
use ringbuf::ringbuf_entry;
use userlib::UnwrapLite;

pub(crate) struct MgsHandler {
    pub(crate) usart: UsartHandler,
    attached_serial_console_mgs: Option<(SocketAddrV6, SpPort)>,
    serial_console_packetizer: SerialConsolePacketizer,
}

impl MgsHandler {
    pub(crate) fn new(usart: UsartHandler) -> Self {
        Self {
            usart,
            attached_serial_console_mgs: None,
            serial_console_packetizer: SerialConsolePacketizer::new(
                // TODO should we remove the "component" from the serial console
                // MGS API? Any chance we ever want to support multiple "serial
                // console"s?
                SpComponent::try_from("sp3").unwrap_lite(),
            ),
        }
    }

    pub(crate) fn needs_usart_flush_to_mgs(&self) -> bool {
        self.usart.should_flush_to_mgs()
    }

    pub(crate) fn flush_usart_to_mgs(
        &mut self,
    ) -> Option<(SerialConsole, SocketAddrV6, SpPort)> {
        // Bail if we don't have any data to flush.
        if !self.needs_usart_flush_to_mgs() {
            return None;
        }

        if let Some((mgs_addr, sp_port)) = self.attached_serial_console_mgs {
            let (serial_console_packet, leftover) = self
                .serial_console_packetizer
                .first_packet(&self.usart.from_rx);

            // Based on the size of `usart.from_rx`, we should never have
            // any leftover data (it holds at most one packet worth).
            assert!(leftover.is_empty());
            self.usart.clear_rx_data();

            Some((serial_console_packet, mgs_addr, sp_port))
        } else {
            // We have data to flush but no attached MGS instance; discard it.
            self.usart.clear_rx_data();
            None
        }
    }
}

impl SpHandler for MgsHandler {
    fn discover(
        &mut self,
        _sender: SocketAddrV6,
        port: SpPort,
    ) -> Result<DiscoverResponse, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::Discovery));
        Ok(DiscoverResponse { sp_port: port })
    }

    fn ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
    ) -> Result<IgnitionState, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::IgnitionState { target }));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<BulkIgnitionState, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::BulkIgnitionState));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn ignition_command(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::IgnitionCommand {
            target,
            command
        }));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn sp_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<SpState, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SpState));

        // TODO Replace with the real serial number once it's available; for now
        // use the stm32 96-bit uid
        let mut serial_number = [0; 16];
        for (to, from) in serial_number.iter_mut().zip(
            drv_stm32xx_uid::read_uid()
                .iter()
                .map(|x| x.to_be_bytes())
                .flatten(),
        ) {
            *to = from;
        }

        Ok(SpState { serial_number })
    }

    fn serial_console_write(
        &mut self,
        sender: SocketAddrV6,
        port: SpPort,
        packet: SerialConsole,
    ) -> Result<(), ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleWrite {
            length: packet.len
        }));

        // TODO check packet.component and/or packet.offset?

        // TODO serial console access should require auth; for now, receiving
        // serial console data implicitly attaches us
        self.attached_serial_console_mgs = Some((sender, port));

        let data = &packet.data[..usize::from(packet.len)];
        if self.usart.tx_buffer_remaining_capacity() >= data.len() {
            self.usart.tx_buffer_append(data);
            Ok(())
        } else {
            Err(ResponseError::Busy)
        }
    }
}
