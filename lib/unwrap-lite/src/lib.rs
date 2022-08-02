// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

/// Extension trait for adding the `unwrap_lite` operation to types that can be
/// unwrapped.
pub trait UnwrapLite {
    /// Type produced when `Self` is unwrapped.
    type Output;

    /// Unwraps `self` without invoking `Debug` formatting, and with a minimal
    /// error message.
    fn unwrap_lite(self) -> Self::Output;
}

impl<T, E> UnwrapLite for Result<T, E> {
    type Output = T;

    #[track_caller]
    #[inline(always)]
    fn unwrap_lite(self) -> Self::Output {
        match self {
            Ok(x) => x,
            Err(_) => panic!(),
        }
    }
}

impl<T> UnwrapLite for Option<T> {
    type Output = T;

    #[track_caller]
    #[inline(always)]
    fn unwrap_lite(self) -> Self::Output {
        match self {
            Some(x) => x,
            None => panic!(),
        }
    }
}
