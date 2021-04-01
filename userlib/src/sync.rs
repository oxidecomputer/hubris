//! Analogs to `std::sync` but specialized to our single-threaded environment.

use core::sync::atomic::{AtomicBool, Ordering};
use core::cell::UnsafeCell;

/// Manages access to a `T`, ensuring that only one observer has access to it at
/// a time. This can be used to put mutable data in a `static` safely.
///
/// In our system, this plays two roles:
///
/// 1. It convinces the compiler to let us put non-`Sync` data in a `static`. We
///    are single threaded, but the compiler does not understand that.
///
/// 2. It catches mistakes like reentrant access to the `static`, which could
///    violate aliasing rules even in a single-threaded environment.
///
/// # Consider whether you need this.
///
/// Even with controlled access, a global is still a global. This type is only
/// appropriate for cases where you genuinely want something to be global
/// (diagnostic ringbuffers are the canonical example). In most cases, if you
/// want static mutable data, you'd be better off with `singleton`.
///
/// # Reentrant access is a programming error.
///
/// This is a lot like a mutex, but it is more like a `RefCell` in practice,
/// because if code tries to access it twice, it panics instead of trying to
/// block. By using this type, you're saying that any attempt to get two
/// references to the contents simultaneously would indicate an error in your
/// program. This is usually what you want with a mutable `static`.
///
/// Note that this type does not provide any sort of read-write-lock facility.
/// It could be extended to do this, but we haven't needed it yet.
pub struct AtomicHolder<T> {
    /// Flag that gets set when the contents may be observed.
    ///
    /// This is `Atomic`, not because we expect threads, but because that
    /// convinces the compiler that it is correctly `Sync` (because it is).
    taken: AtomicBool,
    /// Holder for the contents.
    contents: UnsafeCell<T>,
}

// We assert that AtomicHolder can be shared across "threads" safely if its
// contents can be sent _between_ threads. This is actually somewhat
// conservative for our single-threaded environment, but ensures that the code
// is correct if compiled in a context where there _are_ threads, which we
// can't prevent!
unsafe impl<T: Send> Sync for AtomicHolder<T> {}

impl<T> AtomicHolder<T> {
    /// Creates a holder initialized with `value`.
    ///
    /// This is `const` so you can use it in the initializer for a `static`.
    pub const fn new(value: T) -> Self {
        Self {
            taken: AtomicBool::new(false),
            contents: UnsafeCell::new(value),
        }
    }

    /// Gets a smart-pointer to the contents. The returned handle can be treated
    /// as a mutable reference (i.e. it implements both `Deref` and `DerefMut`),
    /// and will prevent creation of _another_ handle until it has been dropped.
    ///
    /// # Panics
    ///
    /// If you call this while another handle is outstanding, because that would
    /// indicate a bug.
    pub fn take(&self) -> HolderHandle<'_, T> {
        if self.taken.swap(true, Ordering::Acquire) {
            // Double take! Oh no!
            panic!()
        }
        
        // Safety: since we've successfully changed the flag from false to true,
        // there are no other references to our contents outstanding, so we can
        // produce an exclusive/mutable reference.
        unsafe {
            HolderHandle {
                flag_handle: FlagHandle(&self.taken),
                value: &mut *self.contents.get(),
            }
        }
    }
}

/// A smart-pointer "handle" to the contents of an `AtomicHolder`.
///
/// This functions like a mutable reference (i.e. it implements `Deref` and
/// `DerefMut`), but ensures that it's unique at runtime, instead of compile
/// time, by marking the `AtomicHolder` as "taken" until it is dropped.
pub struct HolderHandle<'a, T> {
    flag_handle: FlagHandle<'a>,
    value: &'a mut T,
}

impl<'a, T> HolderHandle<'a, T> {
    pub fn leak(self) -> &'a mut T {
        // Take ourselves apart.
        let HolderHandle { flag_handle, value } = self;
        // Destroy the flag handle without running the drop impl
        core::mem::forget(flag_handle);
        // And now you have a perma-reference
        value
    }
}

impl<T> core::ops::Deref for HolderHandle<'_, T> {
    type Target = T;
    
    fn deref(&self) -> &T {
        self.value
    }
}

impl<T> core::ops::DerefMut for HolderHandle<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value
    }
}

/// Manages a reference to the taken flag and handles clearing it on drop. This
/// is a separate type from `HolderHandle` because we need to be able to take
/// `HolderHandle` apart on `leak`, so it can't have its _own_ `Drop` impl. (If
/// that doesn't make sense, try merging the types and think about the compile
/// errors you get.)
struct FlagHandle<'a>(&'a AtomicBool);

impl Drop for FlagHandle<'_> {
    fn drop(&mut self) {
        // Flip the flag back.
        self.0.store(false, Ordering::Release);
    }
}

/// Macro for declaring some data with `static` lifetime, but initializing it
/// with a potentially non-`const` expression.
///
/// The macro
///
/// ```ignore
/// singleton!(T = expression)
/// ```
///
/// evaluates to a `&'static mut T` initialized from `expression`.
/// Initialization happens when the code containing the macro is first executed.
/// You'd use it like this:
///
/// ```ignore
/// let my_data = singleton!(Something, make_a_something());
/// my_data.field = whatever; // it's mutable
/// ```
///
/// If the code containing the macro is executed _again_, it will panic, because
/// it can't produce _two_ exclusive references to the same data. This is the
/// behavior you want in most simple cases; if you may need to access the data
/// from multiple places in the code, without passing a reference around, you'll
/// need to use `AtomicHolder` directly.
///
/// This is a cognate to the `cortex_m::singleton!` macro, but built around
/// `AtomicHolder` instead of ARM-specific operations that don't work in
/// unprivileged mode anyway.
macro_rules! singleton {
    ($ty:ty = $expr:expr) => {
        // These curly braces are pretty important: they create a local scope
        // for the expression we're going to return. If you remove these it will
        // fail to compile and also be wrong.
        {
            // It would be _super great_ to `use` these types and make them
            // shorter, but that would put them in scope for the evaluation of
            // $ty and $expr, which we don't want. Macros are _almost_ hygienic
            // but not with respect to type lookup.
            static HOLDER:
                $crate::sync::AtomicHolder<core::mem::MaybeUninit<$ty>> =
                $crate::sync::AtomicHolder::new(core::mem::MaybeUninit::uninit());
            
            // Take ownership of the holder's contents and then leak our handle,
            // so that nobody can ever take them again, mwa ha ha ha ha
            let mut handle = HOLDER.take();
            let handle = handle.leak();
            // Initialize. (You could also initialize with a raw pointer write,
            // but this is strictly less unsafe code, so.)
            *handle = core::mem::MaybeUninit::new($expr);
            // Get a reference to the now-initialized memory.
            // Safety: we _just_ initialized this.
            unsafe {
                &mut *handle.as_mut_ptr()
            }
        }
    };
}
