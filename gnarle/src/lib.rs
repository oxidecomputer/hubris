//! A very simple Run-Length Encoding (RLE) compression method.
//!
//! This is mostly intended for compressing data with sections of very low
//! entropy, such as FPGA bitstreams. It generally performs worse than lz4, but
//! there don't appear to be any `no_std` lz4 crates out there, no matter what
//! their READMEs claim.

#![no_std]

use core::convert::TryFrom;

/// Internal definition of how long the run count is. Tuning this might improve
/// performance, though its current value seems optimal in practice.
type RunType = u8;

/// The byte used to signal that data is being interrupted for a run. This value
/// was chosen as a relatively infrequent byte in iCE40 bitstreams. In practice,
/// for the sorts of files we deal in, its value doesn't really matter as long
/// as it isn't `0x00`.
const ESC: u8 = 0xBA;

/// Compresses data from `input`, handing the results to `out` as small slices.
/// `out` has the opportunity to abort compression by returning `Err`. `out` is
/// a function instead of, say, a `&mut [u8]` so that you can choose to write to
/// a file or push to `Vec` in a `std` environment.
///
/// If `out` cannot fail, `compress` will never return `Err`;
/// `std::convert::Infallible` may be the appropriate error type in such cases.
///
/// You can call `compress` more than once to process input in chunks. A
/// sequence of data chopped into arbitrary chunks, compressed, and then
/// concatenated is still a valid RLE sequence, though runs that cross chunk
/// boundaries will be compressed less efficiently.
pub fn compress<E>(
    input: &[u8],
    mut out: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    let mut current_run: Option<(u8, usize)> = None;
    for &byte in input {
        if let Some((current_byte, current_len)) = &mut current_run {
            if byte == *current_byte
                && *current_len < usize::from(RunType::MAX) + 1
            {
                *current_len += 1;
                continue;
            }
            generate_run(*current_byte, *current_len, &mut out)?;
        }

        current_run = Some((byte, 1));
    }
    if let Some((current_byte, current_len)) = current_run {
        generate_run(current_byte, current_len, &mut out)?;
    }

    Ok(())
}

fn generate_run<E>(
    byte: u8,
    count: usize,
    out: &mut impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    if count < 4 && byte != ESC {
        for _ in 0..count {
            out(&[byte])?;
        }
    } else {
        out(&[ESC, byte])?;
        out(&RunType::try_from(count - 1).unwrap().to_le_bytes())?;
    }
    Ok(())
}

/// State that you're expected to hang on to while decompressing something.
pub struct Decompressor(DState);

impl Decompressor {
    pub fn is_idle(&self) -> bool {
        matches!(self.0, DState::Copying)
    }
}

impl Default for Decompressor {
    fn default() -> Self {
        Self(DState::Copying)
    }
}

enum DState {
    /// We're not in a run, we're just copying bytes to the output and watching
    /// for the escape byte.
    Copying,
    /// We're in a run, we are going to produce the given byte N times, where
    /// the count on the right is `N-1`.
    Repeating(u8, RunType),
}

/// Decompresses a chunk of data `input`, writing results to the start of
/// `output`. Returns the prefix of `output` that was written.
///
/// This is intended to be used to incrementally decompress input streams into
/// output buffers. Note that `input` is a `&mut &[u8]` -- `decompress` will
/// update the slice by lopping off the initial bytes that have been consumed.
///
/// Compression stops when we reach the end of either `input` or `output`,
/// whichever comes first.
///
/// - If `input.is_empty()` then the input has been completely consumed.
/// - If `state.is_idle()` too, then there was enough room in `output` for the
///   complete decompressed form. (Otherwise, find or reuse an output buffer and
///   call `decompress(state, &mut &[], output)` until the decompressor becomes
///   idle.)
pub fn decompress<'a>(
    state: &mut Decompressor,
    input: &mut &[u8],
    output: &'a mut [u8],
) -> &'a [u8] {
    fn take_byte(input: &mut &[u8]) -> Option<u8> {
        let (first, rest) = input.split_first()?;
        *input = rest;
        Some(*first)
    }

    let mut n = 0;
    while n < output.len() {
        match &mut state.0 {
            DState::Repeating(byte, count) => {
                output[n] = *byte;
                n += 1;
                if let Some(new_count) = count.checked_sub(1) {
                    *count = new_count;
                } else {
                    state.0 = DState::Copying;
                }
            }
            DState::Copying => match take_byte(input) {
                Some(ESC) => {
                    let actual_byte = take_byte(input);
                    let count = take_byte(input);
                    if let (Some(ab), Some(c)) = (actual_byte, count) {
                        state.0 = DState::Repeating(ab, c);
                    } else {
                        break;
                    }
                }
                Some(byte) => {
                    output[n] = byte;
                    n += 1;
                }
                None => break,
            },
        }
    }

    &output[..n]
}
