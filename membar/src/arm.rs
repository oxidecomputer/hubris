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
        asm!(
            "dmb sy",
            options(nomem, nostack),
        );
    }
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
