// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::mgs_handler::UpdateBuffer;
use gateway_messages::{SpError, UpdateId, UpdateStatus};

#[cfg(any(feature = "gimlet", feature = "grapefruit"))]
pub(crate) mod host_flash;

mod common;
pub(crate) mod rot;
pub(crate) mod sp;

// TODO Currently we only have one implementor of this trait
// (`HostFlashUpdate`), and we don't actually use the trait directly anywhere.
// This is currently just a guide for how we expect component update
// implementations to be structured; once we have more than one, we can adjust
// this trait definition and/or see if it makes sense to have some generic
// support code.
//
// `SpUpdate` has methods with similar names/signatures, but aren't quite the
// same due to having to deal with both auxflash and SP images. It therefore
// does not implement this trait. That's another wart that we might be able to
// address once we start adding more implementors and tweak this trait.
pub(crate) trait ComponentUpdater {
    /// Size of one block / sector / page; whatever unit the underlying update
    /// mechanism wants as a single chunk.
    const BLOCK_SIZE: usize;

    /// Record provided to the `prepare` operation. Generally
    /// `ComponentUpdatePrepare`, and would default to that if associated type
    /// defaults were stable at the time of this writing (2024-04), which they
    /// are not.
    type UpdatePrepare;

    /// Type used to specify sub-components within a component.
    type SubComponent;

    /// Attempt to start preparing for an update, using `buffer` as the backing
    /// store for incoming data.
    ///
    /// Implementors should record the `UpdateId` carried by `update` for future
    /// correlation in `ingest_chunk()` and `abort()`.
    fn prepare(
        &mut self,
        buffer: &'static UpdateBuffer,
        update: Self::UpdatePrepare,
    ) -> Result<(), SpError>;

    /// Returns true if this task needs `step_preparation()` called.
    fn is_preparing(&self) -> bool;

    /// Do a small amount of preparation work.
    fn step_preparation(&mut self);

    /// Status of this update.
    fn status(&self) -> UpdateStatus;

    /// Attempt to ingest a single update chunk from MGS.
    fn ingest_chunk(
        &mut self,
        sub: &Self::SubComponent,
        id: &UpdateId,
        offset: u32,
        data: &[u8],
    ) -> Result<(), SpError>;

    /// Abort the current update if it matches `id`.
    ///
    /// If no update is in progress, should return `Ok(())`; i.e., this should
    /// only return an error if we attempted to abort the update with id `id`
    /// and that abort failed.
    fn abort(&mut self, id: &UpdateId) -> Result<(), SpError>;
}
