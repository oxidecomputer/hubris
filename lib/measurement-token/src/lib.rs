#![no_std]

pub enum MeasurementResult {
    Measured,
    NotMeasured(u64),
}

pub const MEASUREMENT_TOKEN: u64 = 0xc887a12b17ed35f7;
pub const MEASUREMENT_BASE: usize = 0x2000_0000;
const COUNTER_TAG: u64 = 0x4e423d17176f5b51;

extern "C" {
    static mut _HANDOFF_REGION_BASE: [u8; 0];
    static mut _HANDOFF_REGION_END: [u8; 0];
}

pub unsafe fn check_measurement() -> MeasurementResult {
    let ptr: *mut u64 = &raw mut _HANDOFF_REGION_BASE as *mut _;
    let end: *mut u64 = &raw mut _HANDOFF_REGION_END as *mut _;
    assert!(ptr == MEASUREMENT_BASE as *mut _);
    assert!(end.offset_from(ptr) >= 4 * core::mem::size_of::<u64>() as isize);

    let token = core::ptr::read_volatile(ptr);
    let tag = core::ptr::read_volatile(ptr.wrapping_add(1));
    let mut counter = core::ptr::read_volatile(ptr.wrapping_add(2));
    let check = core::ptr::read_volatile(ptr.wrapping_add(3));

    if token == MEASUREMENT_TOKEN {
        clear();
        MeasurementResult::Measured
    } else if tag != COUNTER_TAG || tag ^ counter != check {
        write_counter(ptr, 1);
        MeasurementResult::NotMeasured(0)
    } else {
        counter += 1;
        write_counter(ptr, counter);
        MeasurementResult::NotMeasured(counter)
    }
}

pub unsafe fn clear() {
    let ptr: *mut u64 = &raw mut _HANDOFF_REGION_BASE as *mut _;
    core::ptr::write_volatile(ptr, 0);
    core::ptr::write_volatile(ptr.wrapping_add(1), 0);
}

unsafe fn write_counter(ptr: *mut u64, counter: u64) {
    core::ptr::write_volatile(ptr.wrapping_add(1), COUNTER_TAG);
    core::ptr::write_volatile(ptr.wrapping_add(2), counter);
    core::ptr::write_volatile(ptr.wrapping_add(3), counter ^ COUNTER_TAG);
}
