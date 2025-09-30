// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32H7 Hash low-level driver crate.

// TODO: For the purpose of performance analysis, the hash driver could be
// linked directly with the hf-server or qspi-driver which could then call
// the hash driver directly.
// DMA from regions of the QSPI device to the HASH block may offer
// better performance. Note that the complexity of either of those may not be
// worth possibly marginal performance gains. If we mount an effort to minimize
// boot time, then measuring and attesting to the host firmware may offer some
// opportunity for improvement.
//
// If multiple clients are using the HASH block concurrently, then exclusive
// short-term use and/or saving and restoring of intermediate HASH state
// needs to be supported. The hash server should be able to know all of the
// potential clients and have a context save area for each of them.
// There may be security considerations around such saved state. Care should
// be taken to clear it after it is no longer needed.

#![no_std]

use drv_hash_api::HashError;

// Other SKUs in the STM32 line having the HASH block:
// stm32{f21[57],f4{05,07,27,29,69},f7{45,65,x6,x7,x9},h7{47cm[47],53{,v},b3},l4x6}
#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use core::mem::size_of;
use userlib::*;
use zerocopy::IntoBytes;

enum State {
    Uninitialized = 1,
    Initialized = 2,
    Processing = 3,
    Finalize = 4,
}

// Wrapper for a reference to the register block.
pub struct Hash {
    reg: &'static device::hash::RegisterBlock,
    interrupt: u32,
    state: State,
    block: [u32; 16], // the STM32 hash block has 16 32-bit words.
    idx: usize,       // index into block
    count: usize,     // number of bytes received
    remainder: u32,   // value of partial unprocessed word
    nvalid: u8,       // number of bits in cached partial word
                      // TODO: Resolve contention for the HASH block among multiple clients.
}

const SIZEOF_U32: usize = size_of::<u32>();
const BITS_PER_BYTE: usize = 8;

impl Hash {
    pub fn new(
        reg: &'static device::hash::RegisterBlock,
        interrupt: u32,
    ) -> Self {
        Self {
            reg,
            interrupt,
            state: State::Uninitialized,
            count: 0,
            remainder: 0,
            nvalid: 0,
            block: [0; 16],
            idx: 0,
            // total: 0,
        }
    }

    // The documentation is a bit ambiguous about initialization and
    // NBLW.
    //
    // If one is using DMA, then NBLW must be known at the start because
    // there is no other opportunity to update NBLW.
    //
    // If software is feeding DIN, then the requirement is either
    // that NBLW has to be written before DCAL is set or must be written
    // before the first word of the last block (16 x 32-bit words) is written.
    //
    // The MBED OS has code that staves off suspension of a hash session
    // if there is one more word left in the current update. That could be
    // just for performance reasons, but may also be an implementation that
    // satisfies an undocumented requirement that NBLW be correct before the
    // last word of a hash session is written. When MBED OS resumes a HASH
    // session, HASH block state registers are restored and then NBLW is written
    // separately. That implies that NBLW does not have to be correct at the
    // beginning of the session.
    // So, one can delay writing NBLW iff the last word is not written until
    // finalize is called.
    //
    // If NBLW is maintained as 0 (all bits in the last word are valid),
    // then one can continuously write 32-bits-valid words into DIN and only
    // update NBLW if writing a last partial block or word.
    //
    // TODO: test to see if NBLW must be correct at start of block or only prior
    // to writing last word. Code is simpler and more efficient if NBLW only
    // needs to be written before last word.
    //
    pub fn init_sha256(&mut self) -> Result<(), HashError> {
        self.count = 0;
        self.remainder = 0;
        self.nvalid = 0;
        self.block.iter_mut().for_each(|m| *m = 0);
        self.idx = 0;
        if self.is_busy() {
            while self.is_busy() {}
        }
        unsafe {
            self.reg.cr.modify(|_, w| {
                w.algo1()
                    .set_bit()
                    .lkey()
                    .clear_bit() // n/a when mode=0
                    .mdmat()
                    .clear_bit() // n/a when DMA is not used
                    .algo0()
                    .set_bit() // algo=0b11 is SHA256
                    .mode()
                    .clear_bit() // HASH mode, not HMAC
                    .datatype()
                    .bits(0b10) // 0b10=Write little-endian to DIN
                    .dmae()
                    .clear_bit() // DMA disabled
                    .init()
                    .set_bit()
            });
            // It appears that NBLW just has to be correct when that last
            // word is written.
            // finalize() will set nblw before writing any remainder bytes.
            // All other cases are whole words.
            self.reg.str.modify(|_, w| w.nblw().bits(0));
        }
        self.reg.cr.modify(|_, w| w.init().set_bit());
        self.reg
            .imr
            .modify(|_, w| w.dcie().clear_bit().dinie().clear_bit());

        self.state = State::Initialized;

        Ok(())
    }

    fn write_block(&mut self) {
        // sr.dinis indicates that there is room for a full block
        if self.is_busy() {
            // XXX do i need to check DINIS? || !is_dinis_set() {
            while self.is_busy() {
                // || !is_dinis_set() {
            }
        }

        if self.idx > 0 {
            unsafe {
                // Only the last block can have a partial word at the end.
                // NBLW is initialized to 0 (last word has 32 valid bits) and
                // can stay at zero if that doesn't change.
                self.reg.str.modify(|_, w| {
                    w.nblw()
                        .bits(((self.count % SIZEOF_U32) * BITS_PER_BYTE) as u8)
                });
            }
            for data in &self.block[0..self.idx] {
                // If we were writing word instead of block at a time,
                // then a busy check might be needed here.
                unsafe {
                    self.reg.din.write(|w| w.datain().bits(*data));
                }
            }
            self.idx = 0;
        }
    }

    fn write_word(&mut self, word: u32, valid_bytes: usize) {
        if self.idx >= self.block.len() {
            self.write_block();
        }
        self.block[self.idx] = word;
        self.idx += 1;
        self.count += valid_bytes;
    }

    /// Update hash with additional bytes of data.
    // Little-endian data is fed to the hasher.
    // e.g. "abc" is represented as 0x00636261
    // Only the last data processed by the hasher can be less than 4 bytes.
    pub fn update(&mut self, data: &[u8]) -> Result<(), HashError> {
        match self.state {
            State::Uninitialized => {
                return Err(HashError::NotInitialized);
            }
            State::Initialized => {
                self.state = State::Processing;
            }
            State::Processing => {}
            _ => {
                return Err(HashError::InvalidState);
            }
        };

        // Incoming data might not be aligned.
        // TODO: Test above assumption and optimize if false.
        //
        // From the STM32H7 reference:
        //
        //  "...message string “abc” with a bit string representation of
        //  “01100001 01100010 01100011” is represented by a 32-bit word
        //  0x00636261, and 8-bit words 0x61626300."

        // Deal with the remainder bytes from last update if any.
        let mut offset = 0;
        if self.nvalid > 0 {
            while self.nvalid < 32 {
                if offset >= data.len() {
                    break;
                }
                self.remainder |= (data[offset] as u32) << self.nvalid;
                self.nvalid += 8;
                offset += 1;
            }
            if self.nvalid == 32 {
                self.write_word(self.remainder, SIZEOF_U32);
                self.nvalid = 0;
                self.remainder = 0;
            }
        }

        // Hash all of the whole words available.
        // The words might not be aligned.
        while offset + SIZEOF_U32 <= data.len() {
            self.write_word(
                (data[offset] as u32)
                    | ((data[offset + 1] as u32) << 8)
                    | ((data[offset + 2] as u32) << 16)
                    | ((data[offset + 3] as u32) << 24),
                SIZEOF_U32,
            );
            offset += SIZEOF_U32;
        }
        while offset + SIZEOF_U32 <= data.len() {
            self.write_word(
                u32::from_le_bytes(
                    (&data[offset..offset + SIZEOF_U32]).try_into().unwrap(),
                ),
                SIZEOF_U32,
            );
            offset += SIZEOF_U32;
        }

        if offset < data.len() {
            while offset < data.len() {
                self.remainder |= (data[offset] as u32) << self.nvalid;
                self.nvalid += 8;
                offset += 1;
            }
        }

        Ok(())
    }

    pub fn finalize_sha256(&mut self, out: &mut [u8]) -> Result<(), HashError> {
        match self.state {
            State::Uninitialized => {
                return Err(HashError::NotInitialized);
            }
            State::Processing => {
                self.state = State::Finalize;
            }
            _ => {
                // Trying to run finalize having written no data is an error.
                return Err(HashError::InvalidState);
            }
        };

        if self.nvalid > 0 {
            // There are remainder bits that need to be written.
            self.write_word(self.remainder, (self.nvalid / 8).into());
            self.nvalid = 0;
        }
        if self.idx > 0 {
            self.write_block(); // flush any final block
        }

        // Enable interrupt for sum calculation done.
        self.reg
            .imr
            .modify(|_, w| w.dcie().set_bit().dinie().clear_bit());

        if self.is_busy() {
            while self.is_busy() {}
        }
        sys_irq_control(self.interrupt, true);
        self.reg.str.modify(|_, w| w.dcal().set_bit());

        // wait for calculation to finalize and interrupt
        loop {
            if self.is_busy() {
                while self.is_busy() {}
            }
            sys_recv_notification(self.interrupt);
            if self.reg.sr.read().dcis().bit() {
                break;
            }
            sys_irq_control(self.interrupt, true); // XXX need this again?
        }
        // DCAL is supposedly not clearable by SW.
        // self.reg.str.modify(|_, w| w.dcal().clear_bit());
        self.reg.imr.modify(|_, w| w.dcie().clear_bit());

        // Mark as complete

        // The hash is read out as words into little endian ARM world.
        // Since the bit order needs to be maintained, read as B.E.
        let result = [
            u32::from_be(self.reg.hash_hr[0].read().bits()),
            u32::from_be(self.reg.hash_hr[1].read().bits()),
            u32::from_be(self.reg.hash_hr[2].read().bits()),
            u32::from_be(self.reg.hash_hr[3].read().bits()),
            u32::from_be(self.reg.hash_hr[4].read().bits()),
            u32::from_be(self.reg.hash_hr[5].read().bits()),
            u32::from_be(self.reg.hash_hr[6].read().bits()),
            u32::from_be(self.reg.hash_hr[7].read().bits()),
        ];
        out.clone_from_slice(result.as_bytes());
        Ok(())
    }

    pub fn digest_sha256(
        &mut self,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<(), HashError> {
        // TODO: init() will wipe out the context of a long running hash in
        // progress.
        self.init_sha256()?;
        self.update(input)?;
        self.finalize_sha256(out)
    }

    fn is_busy(&self) -> bool {
        self.reg.sr.read().busy().bit()
    }

    fn _is_dinis_set(&self) -> bool {
        self.reg.sr.read().dinis().bit()
    }
}
