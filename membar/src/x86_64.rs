//! x86-64 barrier operations.
//!
//! These are taken from Doug Lea's cookbook for Java implementations here:
//! http://gee.cs.oswego.edu/dl/jmm/cookbook.html

#[inline(always)]
pub fn arch_specific_load_load() {
    // no-op
}

#[inline(always)]
pub fn arch_specific_load_store() {
    // no-op
}

#[inline(always)]
pub fn arch_specific_store_load() {
    unsafe {
        asm!("mfence", options(nomem, nostack));
    }
}

#[inline(always)]
pub fn arch_specific_store_store() {
    // no-op
}
