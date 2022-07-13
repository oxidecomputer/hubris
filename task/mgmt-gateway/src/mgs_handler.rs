// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Log;
use crate::MgsMessage;
use crate::UsartHandler;
use crate::__RINGBUF;
use gateway_messages::sp_impl::SocketAddrV6;
use gateway_messages::sp_impl::SpHandler;
use gateway_messages::BulkIgnitionState;
use gateway_messages::DiscoverResponse;
use gateway_messages::IgnitionCommand;
use gateway_messages::IgnitionState;
use gateway_messages::ResponseError;
use gateway_messages::SerialConsole;
use gateway_messages::SpPort;
use gateway_messages::SpState;
use ringbuf::ringbuf_entry;

pub(crate) struct MgsHandler<'a> {
    usart: &'a mut UsartHandler,
    attached_serial_console_mgs: Option<(SocketAddrV6, SpPort)>,
}

impl<'a> MgsHandler<'a> {
    pub(crate) fn new(usart: &'a mut UsartHandler) -> Self {
        Self {
            usart,
            attached_serial_console_mgs: None,
        }
    }

    pub(crate) fn attached_serial_console_mgs(
        &self,
    ) -> Option<(SocketAddrV6, SpPort)> {
        self.attached_serial_console_mgs
    }
}

impl SpHandler for MgsHandler<'_> {
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
