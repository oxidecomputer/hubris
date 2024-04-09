// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Jeffrey --- Jefe's little helper

#![no_std]
#![no_main]

use idol_runtime::RequestError;
use task_jefe_api::{DumpAgentError, DumpArea, Jefe};

userlib::task_slot!(JEFE, jefe);

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl {
        jefe: Jefe::from(JEFE.get_task_id()),
        dump_areas: dumptruck::initialize_dump_areas(),

        #[cfg(feature = "background-dump")]
        dump_queue: background_dump::DumpQueue::new(),
    };
    let mut buf = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buf, &mut server);
    }
}

struct ServerImpl {
    jefe: Jefe,
    dump_areas: u32,
}

impl idl::InOrderJeffreyImpl for ServerImpl {
    fn get_dump_area(
        &mut self,
        _msg: &userlib::RecvMessage,
        index: u8,
    ) -> Result<DumpArea, RequestError<DumpAgentError>> {
        dumptruck::get_dump_area(self.dump_areas, index).map_err(|e| e.into())
    }

    fn claim_dump_area(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<DumpArea, RequestError<DumpAgentError>> {
        dumptruck::claim_dump_area(self.dump_areas).map_err(|e| e.into())
    }

    fn reinitialize_dump_areas(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        self.dump_areas = dumptruck::initialize_dump_areas();
        Ok(())
    }

    fn dump_task(
        &mut self,
        _msg: &userlib::RecvMessage,
        task_index: u32,
    ) -> Result<u8, RequestError<DumpAgentError>> {
        // `dump::dump_task` doesn't check the task index, because it's
        // normally called by a trusted source; we'll do it ourself.
        if task_index == 0 {
            // Can't dump the supervisor
            return Err(DumpAgentError::NotSupported.into());
        }
        dumptruck::dump_task(self.dump_areas, task_index as usize)
            .map_err(|e| e.into())
    }

    fn dump_task_region(
        &mut self,
        _msg: &userlib::RecvMessage,
        task_index: u32,
        address: u32,
        length: u32,
    ) -> Result<u8, RequestError<DumpAgentError>> {
        if task_index == 0 {
            return Err(DumpAgentError::NotSupported.into());
        }
        dumptruck::dump_task_region(
            self.dump_areas,
            task_index as usize,
            address,
            length,
        )
        .map_err(|e| e.into())
    }

    fn reinitialize_dump_from(
        &mut self,
        _msg: &userlib::RecvMessage,
        index: u8,
    ) -> Result<(), RequestError<DumpAgentError>> {
        dumptruck::reinitialize_dump_from(self.dump_areas, index)
            .map_err(|e| e.into())
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::DUMP_REQUEST_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if bits & notifications::DUMP_REQUEST_MASK == 0 {
            return;
        }

        let Some(task) = self.jefe.get_background_dump_task() else {
            return;
        };

        // We'll ignore the result of dumping; it could fail
        // if we're out of space, but we don't have a way of
        // dealing with that right now.
        //
        // TODO: some kind of circular buffer?
        _ = dumptruck::dump_task(self.dump_areas, task as usize);

        self.jefe.finish_background_dump(task);
    }
}

////////////////////////////////////////////////////////////////////////////////

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

// And the Idol bits
mod idl {
    use super::*;
    use userlib::FromPrimitive;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
