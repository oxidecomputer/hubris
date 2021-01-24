//! Driver for the LTC4306 I2C mux

use drv_i2c_api::{Port, Segment, ResponseCode};
use drv_stm32h7_i2c::*;

pub fn ltc4306_enable_segment(
    mux: &I2cMux,
    controller: &I2cController,
    port: Port,
    segment: Segment,
    mut enable: impl FnMut(u32),
    mut wfi: impl FnMut(u32),
) -> Result<(), ResponseCode> {
    Err(ResponseCode::SegmentFailed)
}

