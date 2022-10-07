// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use gateway_messages::UpdateId;
use userlib::UnwrapLite;

/// Helper type for all the update implementation state machines.
///
/// Tracks the current update ID, the total update size, and the state of the
/// update. Internally manages ownership of the update state to give the
/// illusion of in-place modification via [`CurrentUpdate::update_state()`].
pub(super) struct CurrentUpdate<State> {
    id: UpdateId,
    total_size: u32,
    // `state` is _always_ `Some(_)`. We only keep it as an `Option` so that we
    // can can move the state out and replace it with a new one (see
    // `update_state()` below).
    state: Option<State>,
}

impl<State> CurrentUpdate<State> {
    pub(super) fn new(id: UpdateId, total_size: u32, state: State) -> Self {
        Self {
            id,
            total_size,
            state: Some(state),
        }
    }

    pub(super) fn id(&self) -> UpdateId {
        self.id
    }

    pub(super) fn total_size(&self) -> u32 {
        self.total_size
    }

    pub(super) fn state(&self) -> &State {
        self.state.as_ref().unwrap_lite()
    }

    pub(super) fn state_mut(&mut self) -> &mut State {
        self.state.as_mut().unwrap_lite()
    }

    #[inline(always)]
    #[allow(dead_code)] // not used by all configurations
    pub(super) fn update_state<F>(&mut self, f: F)
    where
        F: FnOnce(State) -> State,
    {
        self.update_state_with_result(|state| (f(state), ()));
    }

    #[inline(always)]
    pub(super) fn update_state_with_result<F, T>(&mut self, f: F) -> T
    where
        F: FnOnce(State) -> (State, T),
    {
        let state = self.state.take().unwrap_lite();
        let (new_state, t) = f(state);
        self.state = Some(new_state);
        t
    }
}
