// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! "Server" that brings up the FMC and lets you pooooke it.

#![no_std]
#![no_main]

use core::{convert::Infallible, mem::take};
use counters::{count, Count};
use static_cell::ClaimOnceCell;
use sys_api::{Alternate, OutputType, Peripheral, Port, Pull, Speed};
use task_net_api::{
    LargePayloadBehavior, Net, RecvError, SendError, SocketName,
};
use userlib::*;

cfg_if::cfg_if! {
    if #[cfg(feature = "h743")] {
        use stm32h7::stm32h743 as device;
    } else if #[cfg(feature = "h753")] {
        use stm32h7::stm32h753 as device;
    } else {
        compile_error!("missing supported SoC feature");
    }
}

use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{NotificationHandler, RequestError};

task_slot!(SYS, sys);
task_slot!(NET, net);

#[derive(Count, Copy, Clone)]
enum Event {
    RecvPacket,
    RequestRejected,
    Respond,
}

counters::counters!(Event);

static RX_PACKET: ClaimOnceCell<[u8; 1024]> = ClaimOnceCell::new([0; 1024]);
static TX_PACKET: ClaimOnceCell<[u8; 1024]> = ClaimOnceCell::new([0; 1024]);

#[export_name = "main"]
fn main() -> ! {
    // The FMC must be manually initialized in the kernel startup routine!

    // Safety: we're materializing our sole pointer into the FMC controller
    // space, which is fine even if it aliases (which it doesn't).
    let fmc = unsafe { &*device::FMC::ptr() };

    let net = Net::from(NET.get_task_id());

    // Fire up a server.
    let rx_packet = RX_PACKET.claim();
    let tx_packet = TX_PACKET.claim();
    let mut server = ServerImpl {
        fmc,
        net,
        rx_packet,
        tx_packet,
    };
    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    fmc: &'static device::fmc::RegisterBlock,
    net: Net,
    rx_packet: &'static mut [u8; 1024],
    tx_packet: &'static mut [u8; 1024],
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::SOCKET_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if bits & notifications::SOCKET_MASK != 0 {
            const SOCKET: SocketName = SocketName::fmc_test;
            loop {
                match self.net.recv_packet(
                    SOCKET,
                    LargePayloadBehavior::Discard,
                    self.rx_packet,
                ) {
                    Ok(mut meta) => {
                        count!(Event::RecvPacket);
                        let data = &mut self.rx_packet[..meta.size as usize];
                        match process_network_packet(data, self.tx_packet) {
                            Ok(size) => {
                                meta.size = size as u32;
                                count!(Event::Respond);
                            }
                            Err(e) => {
                                count!(Event::RequestRejected);
                                self.tx_packet[0] = e as u8;
                                meta.size = 1;
                            }
                        }
                        let response = &self.tx_packet[..meta.size as usize];
                        // We're going to send best-effort because the only
                        // thing that can really go wrong here is filling our
                        // own queue, which, well, send slower.
                        while let Err(e) =
                            self.net.send_packet(SOCKET, meta, response)
                        {
                            match e {
                                SendError::QueueFull => {
                                    // We've run out of transmit space, which is
                                    // weird, but ok. Wait for it to appear. To
                                    // simplify the code, we won't listen for
                                    // IPCs during this time.
                                    sys_recv_notification(
                                        notifications::SOCKET_MASK,
                                    );
                                }
                                SendError::ServerRestarted => {
                                    // No need to wait.
                                    continue;
                                }
                            }
                        }
                    }
                    Err(RecvError::QueueEmpty) => {
                        // We've got all the packets, go back to handling IPCs.
                        break;
                    }
                    Err(RecvError::ServerRestarted) => {
                        // uh, net stack restarted, chances are good whatever
                        // packet we were reaching for has been dropped.
                        // Nevertheless, try again until it says queue empty.
                    }
                }
            }
        }
    }
}

impl idl::InOrderFmcDemoImpl for ServerImpl {
    fn peek16(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
    ) -> Result<u16, RequestError<Infallible>> {
        let ptr = addr as *const u16;
        let val = unsafe { ptr.read_volatile() };
        Ok(val)
    }

    fn peek32(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
    ) -> Result<u32, RequestError<Infallible>> {
        let ptr = addr as *const u32;
        let val = unsafe { ptr.read_volatile() };
        Ok(val)
    }

    fn peek64(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
    ) -> Result<u64, RequestError<Infallible>> {
        let ptr = addr as *const u64;
        let val = unsafe { ptr.read_volatile() };
        Ok(val)
    }

    fn poke16(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
        value: u16,
    ) -> Result<(), RequestError<Infallible>> {
        let ptr = addr as *mut u16;
        unsafe { ptr.write_volatile(value) }
        Ok(())
    }

    fn poke32(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
        value: u32,
    ) -> Result<(), RequestError<Infallible>> {
        let ptr = addr as *mut u32;
        unsafe { ptr.write_volatile(value) }
        Ok(())
    }

    fn poke64(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
        value: u64,
    ) -> Result<(), RequestError<Infallible>> {
        let ptr = addr as *mut u64;
        unsafe { ptr.write_volatile(value) }
        Ok(())
    }

    fn set_burst_enable(
        &mut self,
        _msg: &RecvMessage,
        flag: bool,
    ) -> Result<(), RequestError<Infallible>> {
        self.fmc.bcr1.modify(|_, w| {
            w.bursten().bit(flag);
            w.cburstrw().bit(flag);
            w
        });
        Ok(())
    }
    fn set_write_enable(
        &mut self,
        _msg: &RecvMessage,
        flag: bool,
    ) -> Result<(), RequestError<Infallible>> {
        self.fmc.bcr1.modify(|_, w| {
            w.wren().bit(flag);
            w
        });
        Ok(())
    }
    fn set_write_fifo(
        &mut self,
        _msg: &RecvMessage,
        flag: bool,
    ) -> Result<(), RequestError<Infallible>> {
        self.fmc.bcr1.modify(|_, w| {
            // NOTE: PARAMETER IS INVERTED
            w.wfdis().bit(!flag);
            w
        });
        Ok(())
    }
    fn set_wait(
        &mut self,
        _msg: &RecvMessage,
        flag: bool,
    ) -> Result<(), RequestError<Infallible>> {
        self.fmc.bcr1.modify(|_, w| {
            w.waiten().bit(flag);
            w
        });
        Ok(())
    }
    fn set_data_latency_cycles(
        &mut self,
        _msg: &RecvMessage,
        n: u8,
    ) -> Result<(), RequestError<Infallible>> {
        let value = n.saturating_sub(2).min(15);
        self.fmc.btr1.write(|w| {
            unsafe {
                w.datlat().bits(value);
            }
            w
        });
        Ok(())
    }
    fn set_clock_divider(
        &mut self,
        _msg: &RecvMessage,
        n: u8,
    ) -> Result<(), RequestError<Infallible>> {
        let value = n.saturating_sub(1).clamp(1, 15);
        self.fmc.btr1.write(|w| {
            unsafe {
                w.clkdiv().bits(value);
            }
            w
        });
        Ok(())
    }
    fn set_bus_turnaround_cycles(
        &mut self,
        _msg: &RecvMessage,
        n: u8,
    ) -> Result<(), RequestError<Infallible>> {
        let value = n.max(15);
        self.fmc.btr1.write(|w| {
            unsafe {
                w.busturn().bits(value);
            }
            w
        });
        Ok(())
    }
}

fn process_network_packet(
    mut packet: &[u8],
    mut response: &mut [u8],
) -> Result<usize, NetworkError> {
    let orig_response_len = response.len();

    let version = read_byte(&mut packet)?;
    let operation = read_byte(&mut packet)?;
    if version != 0 || operation != 0 {
        return Err(NetworkError::NotUnderstood);
    }
    let mut address = 0x6000_0000;

    // Prepare for success
    write_byte(0, &mut response)?;

    while let Ok(byte) = read_byte(&mut packet) {
        match byte {
            0 => {
                // Set Address
                address = u32::from_le_bytes(read_chunk(&mut packet)?) as usize;
            }
            1 | 5 => {
                // Peek8 / Peek8Advance
                let b =
                    unsafe { core::ptr::read_volatile(address as *const u8) };
                write_byte(b, &mut response)?;
                if byte == 5 {
                    address += 1;
                }
            }
            2 | 6 => {
                // Peek16 / Peek16Advance
                let b =
                    unsafe { core::ptr::read_volatile(address as *const u16) };
                write_chunk(b.to_le_bytes(), &mut response)?;
                if byte == 6 {
                    address += 2;
                }
            }
            3 | 7 => {
                // Peek32 / Peek32Advance
                let b =
                    unsafe { core::ptr::read_volatile(address as *const u32) };
                write_chunk(b.to_le_bytes(), &mut response)?;
                if byte == 7 {
                    address += 4;
                }
            }
            4 | 8 => {
                // Peek64 / Peek64Advance
                let b =
                    unsafe { core::ptr::read_volatile(address as *const u64) };
                write_chunk(b.to_le_bytes(), &mut response)?;
                if byte == 8 {
                    address += 8;
                }
            }

            9 | 13 => {
                // Poke8 / Poke8Advance
                let x = read_byte(&mut packet)?;
                unsafe {
                    core::ptr::write_volatile(address as *mut u8, x);
                }
                if byte == 13 {
                    address += 1;
                }
            }
            10 | 14 => {
                // Poke16 / Poke16Advance
                let x = u16::from_le_bytes(read_chunk(&mut packet)?);
                unsafe {
                    core::ptr::write_volatile(address as *mut u16, x);
                }
                if byte == 14 {
                    address += 2;
                }
            }
            11 | 15 => {
                // Poke32 / Poke32Advance
                let x = u32::from_le_bytes(read_chunk(&mut packet)?);
                unsafe {
                    core::ptr::write_volatile(address as *mut u32, x);
                }
                if byte == 15 {
                    address += 4;
                }
            }
            12 | 16 => {
                // Poke64 / Poke64Advance
                let x = u64::from_le_bytes(read_chunk(&mut packet)?);
                unsafe {
                    core::ptr::write_volatile(address as *mut u64, x);
                }
                if byte == 16 {
                    address += 8;
                }
            }
            _ => return Err(NetworkError::NotUnderstood),
        }
    }

    Ok(orig_response_len - response.len())
}

fn read_byte(buf: &mut &[u8]) -> Result<u8, NetworkError> {
    let [byte] = read_chunk(buf)?;
    Ok(byte)
}

fn read_chunk<const N: usize>(
    buf: &mut &[u8],
) -> Result<[u8; N], NetworkError> {
    let (&first, rest) =
        buf.split_first_chunk().ok_or(NetworkError::Truncated)?;
    *buf = rest;
    Ok(first)
}

fn write_byte(byte: u8, buf: &mut &mut [u8]) -> Result<(), NetworkError> {
    write_chunk([byte], buf)
}

fn write_chunk<const N: usize>(
    bytes: [u8; N],
    buf: &mut &mut [u8],
) -> Result<(), NetworkError> {
    let tbuf = take(buf);
    let (first, rest) = tbuf
        .split_first_chunk_mut()
        .ok_or(NetworkError::ResponseTooBig)?;
    *first = bytes;
    *buf = rest;
    Ok(())
}

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
enum NetworkError {
    // please do not use zero here.
    Truncated = 1,
    NotUnderstood = 2,
    ResponseTooBig = 3,
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
