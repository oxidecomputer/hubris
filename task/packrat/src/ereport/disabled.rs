// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! No-op `Ereports` impl, for when the "ereport" feature is not enabled.

use super::Ereports;
use crate::ereport_messages;
use idol_runtime::{Leased, LenLimit, RequestError};
use task_packrat_api::{EreportReadError, EreportWriteError, OxideIdentity};
use userlib::{NotificationBits, RecvMessage};

pub(crate) struct NoEreports {}

impl Ereports for NoEreports {
    fn new() -> Self {
        Self {}
    }

    fn deliver_encoded_ereport(
        &mut self,
        _: &RecvMessage,
        _: LenLimit<Leased<idol_runtime::R, [u8]>, 1024>,
    ) -> Result<ereport_messages::Ena, RequestError<EreportWriteError>> {
        // go away, we don't know how to do that
        Err(idol_runtime::ClientError::UnknownOperation.fail())
    }

    fn read_ereports(
        &mut self,
        _: &Option<ereport_messages::RestartId>,
        _: ereport_messages::RequestIdV0,
        _: ereport_messages::RestartId,
        _: ereport_messages::Ena,
        _: u8,
        _: ereport_messages::Ena,
        _: Leased<idol_runtime::W, [u8]>,
        _: Option<&OxideIdentity>,
    ) -> Result<usize, RequestError<EreportReadError>> {
        // go away, we don't know how to do that
        Err(idol_runtime::ClientError::UnknownOperation.fail())
    }

    // If we are not built with ereport support, we expect no notifications.
    fn notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: NotificationBits) {
        unreachable!()
    }
}
