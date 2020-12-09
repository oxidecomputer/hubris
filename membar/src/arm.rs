//! ARM (i.e. the old 32-bit architecture) barrier operations.
//!
//! Memory accesses that don't impact the pipeline, pagetable, etc. can be
//! entirely handled with the aptly-named Data Memory Barrier instruction, or
//! DMB. One can technically express finer-grained barriers than we do here (we
//! use `dmb sy` meaning "all memory operations, full system"), but the little
//! M-profile ARMs we target only support the coarse grained version. And so, we
//! hit all our problems with the same hammer.

#[inline(always)]
fn dmb() {
    unsafe {
        // We have omitted `nomem`, even though this technically does not access
        // memory, to try to prevent motion of memory accesses across this
        // instruction. Does this work? Who knows! The current inline asm spec
        // does not say.
        asm!("dmb sy", options(nostack, preserves_flags));
    }
    // This is pretty belt-and-suspenders, and like a real belt and suspenders,
    // it's an odd combination that looks silly. We have included this compiler
    // fence out of an abundance of caution, but one of two things must be true:
    //
    // 1. Something prevents the compiler from reordering memory accesses across
    //    the `asm!` above, in which case we don't need the fence.
    //
    // 2. Somethin _doesn't,_ in which case the compiler can move accesses
    //    _between_ the `asm!` and the fence, and we cannot achieve our desired
    //    semantics and have lost the game.
    //
    // Let's hope it's #1 and we can remove the fence.
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

#[inline(always)]
pub fn arch_specific_load_load() {
    dmb();
}

#[inline(always)]
pub fn arch_specific_load_store() {
    dmb();
}

#[inline(always)]
pub fn arch_specific_store_load() {
    dmb();
}

#[inline(always)]
pub fn arch_specific_store_store() {
    dmb();
}
