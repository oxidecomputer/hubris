//! x86-64 barrier operations.
//!
//! If anyone finds a concise definitive mapping of SSE2 barrier instructions to
//! the terminology used in this crate, please link it here. Until then, here is
//! a list of resources that partially helped if you overlap them just right.
//!
//! https://stackoverflow.com/questions/20316124/does-it-make-any-sense-to-use-the-lfence-instruction-on-x86-x86-64-processors
//! https://stackoverflow.com/questions/27627969/why-is-or-isnt-sfence-lfence-equivalent-to-mfence

// Note: you might notice a whole bunch of `compiler_fence` in the
// implementation below. It's not totally clear whether these are necessary. See
// the longer comment in src/arm.rs for details.

#[inline(always)]
pub fn arch_specific_load_load() {
    unsafe {
        asm!("lfence", options(nostack));
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

#[inline(always)]
pub fn arch_specific_load_store() {
    unsafe {
        asm!("lfence", options(nostack));
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

#[inline(always)]
pub fn arch_specific_store_load() {
    unsafe {
        asm!("mfence", options(nostack));
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

#[inline(always)]
pub fn arch_specific_store_store() {
    unsafe {
        asm!("sfence", options(nostack));
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}
