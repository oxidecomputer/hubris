// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Receives host sensor data via UDP and injects it into the sensor framework.
//!
//! This task listens on a UDP socket for sensor data packets from an external
//! host (e.g., an N5105 NAS). When packets arrive, the contained sensor
//! readings are posted to the Hubris sensor task via IPC, making them visible
//! through the standard sensor infrastructure (including faux-mgs).
//!
//! If no packets are received within the timeout period, sensors are marked
//! as NoData::DeviceTimeout.

#![no_std]
#![no_main]

use task_net_api::*;
use task_sensor_api::{NoData, Sensor, SensorId};
use userlib::*;

task_slot!(NET, net);
task_slot!(SENSOR, sensor);

/// Protocol magic byte
const MAGIC: u8 = 0x53; // 'S'

/// Protocol version
const VERSION: u8 = 0x01;

/// Header size in bytes
const HEADER_SIZE: usize = 4;

/// Record size in bytes (1 byte channel + 4 bytes f32 LE)
const RECORD_SIZE: usize = 5;

/// Maximum number of sensors we support injecting.
/// Must match the number of virtual i2c sensor devices in the app TOML.
const NUM_INJECT_SENSORS: usize = 3;

/// Timeout in milliseconds. If no packet is received within this duration,
/// all sensors are marked as NoData::DeviceTimeout.
const TIMEOUT_MS: u64 = 10_000;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Trace {
    None,
    Start,
    PacketReceived { num_records: u8 },
    SensorPosted { channel: u8 },
    BadMagic { got: u8 },
    BadVersion { got: u8 },
    BadChannel { channel: u8 },
    PacketTooShort { len: usize },
    Timeout,
}

ringbuf::ringbuf!(Trace, 32, Trace::None);

static INJECT_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

#[export_name = "main"]
fn main() -> ! {
    let net = Net::from(NET.get_task_id());
    let sensor = Sensor::from(SENSOR.get_task_id());

    const SOCKET: SocketName = SocketName::sensor_inject;

    ringbuf::ringbuf_entry!(Trace::Start);

    // Track whether we have ever received a packet (for timeout logic).
    // We do not mark sensors as timed out until we have received at least
    // one packet, to avoid spurious NoData at boot before the sender starts.
    let mut ever_received = false;
    let mut last_received_time: u64 = 0;

    let mut rx_buf = [0u8; 256];

    loop {
        // Check for timeout: if we previously received data but haven't
        // received anything recently, mark all sensors as timed out.
        if ever_received {
            let now = sys_get_timer().now;
            if now.wrapping_sub(last_received_time) > TIMEOUT_MS {
                for ch in 0..NUM_INJECT_SENSORS {
                    if let Ok(id) = SensorId::try_from(ch as u32) {
                        sensor.nodata_now(id, NoData::DeviceTimeout);
                    }
                }
                ringbuf::ringbuf_entry!(Trace::Timeout);
                // Reset so we don't spam nodata every loop iteration.
                ever_received = false;
            }
        }

        match net.recv_packet(
            SOCKET,
            LargePayloadBehavior::Discard,
            &mut rx_buf,
        ) {
            Ok(meta) => {
                let len = meta.size as usize;
                if let Some(n) = process_packet(&rx_buf[..len], &sensor) {
                    INJECT_COUNT.fetch_add(
                        n as u32,
                        core::sync::atomic::Ordering::Relaxed,
                    );
                    last_received_time = sys_get_timer().now;
                    ever_received = true;
                }
            }
            Err(RecvError::QueueEmpty) => {
                // No packet available. Set a timer and wait for either
                // a socket notification or timer expiry (for timeout check).
                let deadline = sys_get_timer().now + 1_000;
                sys_set_timer(Some(deadline), notifications::TIMER_MASK);
                sys_recv_notification(
                    notifications::SOCKET_MASK | notifications::TIMER_MASK,
                );
            }
            Err(RecvError::ServerRestarted) => {
                // net restarted, just retry
            }
        }
    }
}

/// Parse a sensor-inject packet and post readings to the sensor task.
/// Returns Some(count) of successfully posted readings, or None on
/// header parse error.
fn process_packet(data: &[u8], sensor: &Sensor) -> Option<u8> {
    if data.len() < HEADER_SIZE {
        ringbuf::ringbuf_entry!(Trace::PacketTooShort { len: data.len() });
        return None;
    }

    let magic = data[0];
    if magic != MAGIC {
        ringbuf::ringbuf_entry!(Trace::BadMagic { got: magic });
        return None;
    }

    let version = data[1];
    if version != VERSION {
        ringbuf::ringbuf_entry!(Trace::BadVersion { got: version });
        return None;
    }

    let num_records = data[2];
    // data[3] is reserved

    ringbuf::ringbuf_entry!(Trace::PacketReceived { num_records });

    let payload = &data[HEADER_SIZE..];
    let mut posted = 0u8;

    for i in 0..num_records as usize {
        let offset = i * RECORD_SIZE;
        if offset + RECORD_SIZE > payload.len() {
            break; // Truncated record, stop processing
        }

        let channel = payload[offset];
        let value_bytes: [u8; 4] = [
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
            payload[offset + 4],
        ];
        let value = f32::from_le_bytes(value_bytes);

        // Validate the channel maps to a valid SensorId within our range
        if (channel as usize) < NUM_INJECT_SENSORS {
            if let Ok(id) = SensorId::try_from(channel as u32) {
                sensor.post_now(id, value);
                ringbuf::ringbuf_entry!(Trace::SensorPosted { channel });
                posted += 1;
            }
        } else {
            ringbuf::ringbuf_entry!(Trace::BadChannel { channel });
        }
    }

    Some(posted)
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
