// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_i2c_devices::bmr491::*;
use ringbuf::*;

const RAW_VIN_TRACE_DEPTH: usize = 500;

ringbuf!(Trace, 500, Trace::None);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    RawVinSample(u16),
    RawVinSampled(u16),
    RawVin(u16),
    None,
}

pub(crate) enum TraceCount {
    Once,
    Many(u16),
    Fill,
}

pub(crate) fn trace_raw_vin(device: &Bmr491, trace: TraceCount) {
    _ = device.set_telemetry_raw(true);

    match trace {
        TraceCount::Many(_) | TraceCount::Fill => {
            let nsamples = if let TraceCount::Many(nsamples) = trace {
                nsamples
            } else {
                RAW_VIN_TRACE_DEPTH as u16
            };

            for _ in 0..nsamples {
                if let Ok(val) = device.read_vin_raw() {
                    ringbuf_entry!(Trace::RawVinSample(val));
                }
            }

            ringbuf_entry!(Trace::RawVinSampled(nsamples));
        }

        TraceCount::Once => {
            if let Ok(val) = device.read_vin_raw() {
                ringbuf_entry!(Trace::RawVin(val));
            }
        }
    }

    _ = device.set_telemetry_raw(false);
}
