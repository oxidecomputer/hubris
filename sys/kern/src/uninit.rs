//! Utility code for dealing with uninitialized memory safely.

use core::mem::MaybeUninit;

/// Trait implemented by types that contain uninitialized data, and are capable
/// of "pushing" the `MaybeUninit` down in their structure safely.
///
/// # Safety
///
/// To implement this trait safely, it must be legit to reinterpret `&mut Self`
/// as `&mut Self::Output` without additional initialization required.
///
/// For the implementation to further be _reasonable,_ `Self::Output` should
/// have the same structure as `Self` but with a `MaybeUninit` pushed one level
/// deeper into the structure.
pub unsafe trait Unbundle {
    type Output;
}

/// "Push" the `MaybeUninit` in `B` deeper by one level.
pub(crate) fn unbundle<B>(bundle: &mut B) -> &mut B::Output
where
    B: Unbundle,
{
    unsafe { &mut *(bundle as *mut B as *mut B::Output) }
}

// Safety:
// - MaybeUninit<T> is specified as having the same memory layout as T.
// - MaybeUninit<[T; N]> thus has the same layout as
//   MaybeUninit<[MaybeUninit<T>; N]>.
// - We can safely transmute from MaybeUninit<[X; N]> to
//   MaybeUninit<[MaybeUninit<X>; N]> (adding a MaybeUninit wrapper) because
//   the layout is unchanged and it makes no additional assumptions about
//   initialization.
// - We can safely transmute from MaybeUninit<[MaybeUninit<T>; N]> to
//   [MaybeUninit<T>; N] because the layout is identical and it doesn't
//   assume initialization of anything new (the contents are still entirely
//   in MaybeUninit).
unsafe impl<T, const N: usize> Unbundle for MaybeUninit<[T; N]> {
    type Output = [MaybeUninit<T>; N];
}

// Safety:
// - B's impl of Unbundle guarantees that reinterpretation is safe
// - Thus reinterpretation of an array is safe.
unsafe impl<B, const N: usize> Unbundle for [B; N]
where
    B: Unbundle,
{
    type Output = [B::Output; N];
}

/// Trait implemented by types that contain uninitialized data that will be
/// initialized (not shown here) and then reinterpreted as initialized,
/// analogous to `MaybeUninit::assume_init` but more general.
///
/// # Safety
///
/// - `Self::Initialized` must have identical layout to `Self`.
/// - `Self::Initialized` must only assume properties of its contents that would
///   be fulfilled by initializing all `MaybeUninit`s transitively contained
///   within `Self`. A more concrete way of thinking about this is that
///   `Self::Initialized` should differ from `Self` only by the removal of some
///   `MaybeUninit` wrappers.
///
/// See the docs for `MaybeUninit::assume_init` for more details.
///
/// Here is an implementation that is **wrong and bad** under the second
/// criterion:
///
/// ```rust
/// // BAD because bool has a narrower set of possible values than u8
/// unsafe impl AssumeInit for MaybeUninit<u8> {
///     type Initialized = bool; // BAD
/// }
/// ```
pub unsafe trait AssumeInit {
    /// The fully initialized equivalent to `Self`.
    type Initialized;
}

/// Reinteprets a reference to (fully or partially) uninitialized type `A` as a
/// reference to its initialized form.
///
/// # Safety
///
/// This is only safe if you have _fully_ initialized all data in `stuff` that
/// will be revealed as initialized by conversion to `A::Initialized`. In the
/// common case, this means that you've written all the `MaybeUninit`s
/// transitively contained in `stuff` with valid data.
pub(crate) unsafe fn assume_init_mut<A>(stuff: &mut A) -> &mut A::Initialized
where
    A: AssumeInit,
{
    unsafe { &mut *(stuff as *mut A as *mut A::Initialized) }
}

/// Shared reference version of `assume_init_mut`; see its docs for more.
///
/// # Safety
///
/// See `assume_init_mut`.
pub(crate) unsafe fn assume_init_ref<A>(stuff: &A) -> &A::Initialized
where
    A: AssumeInit,
{
    unsafe { &*(stuff as *const A as *const A::Initialized) }
}

// Safety: this is equivalent to `MaybeUninit::assume_init`.
unsafe impl<T> AssumeInit for MaybeUninit<T> {
    type Initialized = T;
}

// Safety: if all elements of the array are initialized, we can treat the array
// as initialized.
unsafe impl<A: AssumeInit, const N: usize> AssumeInit for [A; N] {
    type Initialized = [A::Initialized; N];
}
