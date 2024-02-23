// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#[derive(ringbuf::Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum MainEvent {
    #[count(skip)]
    None,

    /// This generates a counter for each variant of `Person`.
    #[count(children)]
    Person(Person),

    /// This generates a counter for each variant of `Place`.
    #[count(children)]
    Place(Place),

    /// This generates a single counter for `Number`.
    Number(u32),
}

#[derive(ringbuf::Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Person {
    Cliff,
    Matt,
    Laura,
    Bryan,
    John,
    Steve,
}

#[derive(ringbuf::Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Place {
    Emeryville,
    Cambridge,
    Austin,
    SanFrancisco,
    ZipCode(u16),
}

ringbuf::counted_ringbuf!(MainEvent, 16, MainEvent::None);

fn main() {}
