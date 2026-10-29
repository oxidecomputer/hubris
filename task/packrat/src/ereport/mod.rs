// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::ereport_messages;
use idol_runtime::{Leased, LenLimit, RequestError};
use task_packrat_api::{EreportReadError, EreportWriteError, OxideIdentity};
use userlib::{NotificationBits, RecvMessage};

// Select an impl and re-export based on whether ereport support is enabled.
//
// This is a simpler version of what is done for `Bsp` and `BspImpl`. This is
// not bundled into `Bsp` directly as `ereports` is a separately-enabled
// feature.
cfg_if::cfg_if! {
    if #[cfg(feature = "ereport")] {
        mod store;
        pub(crate) use store::EreportStore as EreportsImpl;
    } else {
        mod disabled;
        pub(crate) use disabled::NoEreports as EreportsImpl;
    }
}

/// Packrat's ereport functionality.
///
/// This captures the ereport-specific behavior that either needs to be
/// implemented (if ereports are enabled) or not. In most/all cases, this
/// will just be either the "actually do ereports" impl,
/// [`store::EreportStore`], or the "don't actually do ereports" impl,
/// [`disabled::NoEreports`].
pub(crate) trait Ereports {
    /// Constructs the ereport subsystem, claiming any static storage it
    /// requires.
    ///
    /// This may only be called once.
    fn new() -> Self;

    /// Records the CBOR-encoded ereport in `data`, returning its ENA.
    fn deliver_encoded_ereport(
        &mut self,
        msg: &RecvMessage,
        data: LenLimit<Leased<idol_runtime::R, [u8]>, 1024>,
    ) -> Result<ereport_messages::Ena, RequestError<EreportWriteError>>;

    /// Flushes ereports up to `committed_ena`, and then reads ereports
    /// starting at `begin_ena` into `data`, returning the number of bytes
    /// written.
    #[allow(clippy::too_many_arguments)]
    fn read_ereports(
        &mut self,
        current_restart_id: &Option<ereport_messages::RestartId>,
        request_id: ereport_messages::RequestIdV0,
        restart_id: ereport_messages::RestartId,
        begin_ena: ereport_messages::Ena,
        limit: u8,
        committed_ena: ereport_messages::Ena,
        data: Leased<idol_runtime::W, [u8]>,
        vpd: Option<&OxideIdentity>,
    ) -> Result<usize, RequestError<EreportReadError>>;

    /// The ereport notification mask.
    fn notification_mask(&self) -> u32;

    /// Handles a notification from [`Ereports::notification_mask`].
    fn handle_notification(&mut self, bits: NotificationBits);
}
