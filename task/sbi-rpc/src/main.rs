// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! RPC access to SBI (SB-RMI and SB-TSI) SP3 interfaces.

#![no_std]
#![no_main]

use nprpc::{Request, autobuffer, compose_interfaces};
use nprpc_net::{NetBackend, UdpSocket};
use static_cell::ClaimOnceCell;
use task_net_api::SocketName;
use task_sbi_rpc_api::{SbRmiCallError, sp3_sb_rmi, sp3_sb_tsi};
use userlib::{hl::sleep_for, sys_get_timer, task_slot};

#[cfg(feature = "unsafe-raw-commands")]
use task_sbi_rpc_api::{
    SbRmiCallWriteReadRequest, SbTsiWriteRequest, sp3_raw_sb_rmi,
    sp3_raw_sb_tsi,
};

task_slot!(NET, net);
task_slot!(I2C, i2c_driver);

// Automatically sized buffers
autobuffer!(ApiBufs, composite);

struct ServerImpl;

// If these start failing, go bump the size of the buffers in various app tomls
const _: () = {
    let socket_idx = task_net_api::SocketName::sbirpc as usize;
    assert!(ApiBufs::_RQST_SIZE < task_net_api::SOCKET_RX_SIZE[socket_idx]);
    assert!(ApiBufs::_RESP_SIZE < task_net_api::SOCKET_TX_SIZE[socket_idx]);
};

// This has only been checked for SP3-based sleds, e.g. `gimlet`. Other
// processor families should be reviewed for SBI changes. If we want to support
// other processors, we probably need a `bsp` selection here instead.
const _: () = {
    let is_sp3 = cfg!(target_board = "gimlet-b")
        || cfg!(target_board = "gimlet-c")
        || cfg!(target_board = "gimlet-d")
        || cfg!(target_board = "gimlet-e")
        || cfg!(target_board = "gimlet-f");
    assert!(is_sp3, "Not an SP3-based device!");
};

// Combine interfaces into a single composite interface, serving requests from
// all of the given interfaces.
//
// TODO: Have one version for `-dev` that includes the `raw` commands, and
// another that only accepts a pre-enumerated list.
#[cfg(feature = "unsafe-raw-commands")]
compose_interfaces! {
    mod: composite,
    interfaces: [
        task_sbi_rpc_api::sp3_raw_sb_rmi,
        task_sbi_rpc_api::sp3_raw_sb_tsi,
        task_sbi_rpc_api::sp3_sb_tsi,
        task_sbi_rpc_api::sp3_sb_rmi,
    ]
}

#[cfg(not(feature = "unsafe-raw-commands"))]
compose_interfaces! {
    mod: composite,
    interfaces: [
        task_sbi_rpc_api::sp3_sb_tsi,
        task_sbi_rpc_api::sp3_sb_rmi,
    ]
}

/// Check a condition until the given timeout time, sleeping for 5ms between
/// checks.
fn sbrmi_wait_for<F>(timeout: u64, is_done: F) -> Result<(), SbRmiCallError>
where
    F: Fn() -> Result<bool, SbRmiCallError>,
{
    loop {
        if is_done()? {
            return Ok(());
        }
        if sys_get_timer().now >= timeout {
            return Err(SbRmiCallError::Timeout);
        }
        // Random guess of a reasonable sampling time
        sleep_for(5);
    }
}

fn tsi_helper(reg: u8) -> Result<u8, u32> {
    let task = I2C.get_task_id();
    let sbtsi = crate::i2c_config::devices::sbtsi_cpu(task);
    sbtsi.read_reg(reg).map_err(|code| code as u32)
}

impl sp3_sb_tsi::Server for ServerImpl {
    fn read_cpu_temp(&mut self, _: Request<'_, ()>) -> Result<f32, u32> {
        let task = I2C.get_task_id();
        let sbtsi = crate::i2c_config::devices::sbtsi_cpu(task);
        // integer
        let itg: u8 = sbtsi.read_reg(0x01).map_err(|code| code as u32)?;
        // upper 3 bits increment of 0.125
        let dec: u8 = sbtsi.read_reg(0x10).map_err(|code| code as u32)?;
        Ok((itg as f32) + (((dec >> 5) as f32) / 8.0))
    }

    fn read_status(&mut self, _: Request<'_, ()>) -> Result<u8, u32> {
        tsi_helper(0x02)
    }

    fn read_config(&mut self, _: Request<'_, ()>) -> Result<u8, u32> {
        tsi_helper(0x03)
    }

    fn read_update_rate(&mut self, _: Request<'_, ()>) -> Result<u8, u32> {
        tsi_helper(0x04)
    }

    fn read_high_temp_threshold(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<f32, u32> {
        let task = I2C.get_task_id();
        let sbtsi = crate::i2c_config::devices::sbtsi_cpu(task);
        // integer
        let itg: u8 = sbtsi.read_reg(0x07).map_err(|code| code as u32)?;
        // upper 3 bits increment of 0.125
        let dec: u8 = sbtsi.read_reg(0x13).map_err(|code| code as u32)?;
        Ok((itg as f32) + (((dec >> 5) as f32) / 8.0))
    }

    fn read_low_temp_threshold(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<f32, u32> {
        let task = I2C.get_task_id();
        let sbtsi = crate::i2c_config::devices::sbtsi_cpu(task);
        // integer
        let itg: u8 = sbtsi.read_reg(0x08).map_err(|code| code as u32)?;
        // upper 3 bits increment of 0.125
        let dec: u8 = sbtsi.read_reg(0x14).map_err(|code| code as u32)?;
        Ok((itg as f32) + (((dec >> 5) as f32) / 8.0))
    }

    fn read_timeout_config(&mut self, _: Request<'_, ()>) -> Result<u8, u32> {
        tsi_helper(0x22)
    }

    fn read_alert_threshold(&mut self, _: Request<'_, ()>) -> Result<u8, u32> {
        tsi_helper(0x32)
    }

    fn read_alert_config(&mut self, _: Request<'_, ()>) -> Result<u8, u32> {
        tsi_helper(0xBF)
    }

    fn read_manufacture_id(&mut self, _: Request<'_, ()>) -> Result<u8, u32> {
        tsi_helper(0xFE)
    }

    fn read_revision(&mut self, _: Request<'_, ()>) -> Result<u8, u32> {
        tsi_helper(0xFF)
    }
}

impl sp3_sb_rmi::Server for ServerImpl {
    fn read_package_power_consumption(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x01, None)
    }

    fn read_package_power_limit(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x03, None)
    }

    fn read_max_package_power_limit(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x04, None)
    }

    fn read_tdp(&mut self, _: Request<'_, ()>) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x05, None)
    }

    fn read_max_ctdp(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x06, None)
    }

    fn read_min_ctdp(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x07, None)
    }

    fn read_bios_boost_fmax(
        &mut self,
        rqst: Request<'_, u16>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x08, Some((*rqst.body as u32) << 16))
    }

    fn read_apml_boost_limit(
        &mut self,
        rqst: Request<'_, u16>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x09, Some((*rqst.body as u32) << 16))
    }

    fn read_dram_throttle(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x0C, None)
    }

    fn read_prochot_status(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x0E, None)
    }

    fn read_prochot_residency(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x0F, None)
    }

    fn read_vddio_mem_power(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x10, None)
    }

    fn read_nbio_error_logging(
        &mut self,
        rqst: Request<'_, u32>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x11, Some(*rqst.body))
    }

    fn read_iod_bist_result(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x13, None)
    }

    fn read_ccd_bist_result(
        &mut self,
        rqst: Request<'_, u32>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x14, Some(*rqst.body))
    }

    fn read_ccx_bist_result(
        &mut self,
        rqst: Request<'_, u32>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x15, Some(*rqst.body))
    }

    fn read_max_ddr_bw_util(
        &mut self,
        _: Request<'_, ()>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(0x18, None)
    }
}

/// Perform a "SB-RMI Mailbox Sequence" as per AMD's PPR.
///
/// Works with or without a 32-bit arg.
///
/// See https://docs.amd.com/v/u/en-US/55898_B1_pub_0_50.
fn smm_helper32(cmd: u8, arg: Option<u32>) -> Result<u32, SbRmiCallError> {
    let task = I2C.get_task_id();
    let sbrmi = crate::i2c_config::devices::sbrmi_rmi(task);

    // Read a 1-byte value from the given register
    let byte_read = |reg: u8| -> Result<u8, SbRmiCallError> {
        sbrmi
            .read_reg(reg)
            .map_err(|code| SbRmiCallError::I2cError { code: code as u32 })
    };

    // Write a 1-byte value to the given register
    let byte_write = |reg: u8, val: u8| -> Result<(), SbRmiCallError> {
        sbrmi
            .write(&[reg, val])
            .map_err(|code| SbRmiCallError::I2cError { code: code as u32 })
    };

    // The sequence is as follows:
    //
    // 1. The initiator (BMC) indicates that command is to be serviced by
    //    firmware by writing 80h to SBRMI::InBndMsg_inst7 (SBRMI_x3F). This
    //    register must be set to 80h after reset.
    byte_write(0x3F, 0x80)?;

    // 2. The initiator (BMC) writes the command to SBRMI::InBndMsg_inst0
    //    (SBRMI_x38).
    byte_write(0x38, cmd)?;

    // 3. For Write operations or Read operations, which require additional
    //    addressing information as shown in Table 127 (SB-RMI Soft Mailbox
    //    Message) above, the initiator (BMC) writes Command Data In[31:0]
    //    to SBRMI::InBndMsg_inst[4:1] {SBRMI_x3C(MSB):SBRMI_x39(LSB)}.
    if let Some(arg) = arg {
        let [a, b, c, d] = arg.to_le_bytes();
        byte_write(0x39, a)?; // LSB
        byte_write(0x3A, b)?;
        byte_write(0x3B, c)?;
        byte_write(0x3C, d)?; // MSB
    }

    // 4. The initiator (BMC) writes 01h to SBRMI::SoftwareInterrupt to
    //    notify firmware to perform the requested Read or Write command.
    byte_write(0x40, 0x01)?;

    // For now, assume that 500ms is too long.
    let now = sys_get_timer().now;
    let timeout = now.saturating_add(500);

    // 5. Firmware reads the message and performs the defined action.
    // ...
    // 6. Firmware writes the original command to outbound message register
    //    SBRMI::OutBndMsg_inst0 (SBRMI_x30).
    sbrmi_wait_for(timeout, || Ok(byte_read(0x32)? == cmd))?;

    // 7. Firmware writes SBRMI::Status[SwAlertSts] = 1 to generate an
    //    ALERT (if enabled) to initiator (BMC) to indicate completion of
    //    the requested command. Firmware must (if applicable) put the
    //    message data into the message registers
    //    SBRMI::OutBndMsg_inst[4:1] {SBRMI_x34(MSB):SBRMI_x31(LSB)}.
    sbrmi_wait_for(timeout, || {
        let val = byte_read(0x02)?;
        // SwAlertSts is bit 1
        Ok((val & 0x02) != 0)
    })?;

    // 8. Firmware clears the interrupt on SBRMI::SoftwareInterrupt.
    sbrmi_wait_for(timeout, || {
        let val = byte_read(0x40)?;
        // SwInt is bit 0
        Ok((val & 0x01) == 0)
    })?;

    // 9. For a Read operation, the initiator (BMC) reads the firmware
    //    response Command Data Out[31:0] from
    //    SBRMI::OutBndMsg_inst[4:1] {SBRMI_x34(MSB):SBRMI_x31(LSB)}.
    let payload = [
        byte_read(0x31)?, // LSB
        byte_read(0x32)?,
        byte_read(0x33)?,
        byte_read(0x34)?, // MSB
    ];
    let payload = u32::from_le_bytes(payload);
    let mb_errcode = byte_read(0x37)?;

    // 10. BMC must write 1'b1 to SBRMI::Status[SwAlertSts] to clear the
    //     ALERT to initiator (BMC). It is recommended to clear the ALERT
    //     upon completion of the current mailbox command.
    //
    // TODO: We should probably on_drop this so it happens regardless.
    byte_write(0x02, 0x02)?;

    match mb_errcode {
        0x00 => Ok(payload),
        0x01 => Err(SbRmiCallError::CmdAborted { payload }),
        0x02 => Err(SbRmiCallError::CmdUnknown { payload }),
        0x03 => Err(SbRmiCallError::CmdBadCore { payload }),
        code => Err(SbRmiCallError::UnexpectedStatus { payload, code }),
    }
}

#[cfg(feature = "unsafe-raw-commands")]
impl sp3_raw_sb_rmi::Server for ServerImpl {
    fn smm_read_word(
        &mut self,
        rqst: Request<'_, u8>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(*rqst.body, None)
    }

    fn smm_write_read_word(
        &mut self,
        rqst: Request<'_, SbRmiCallWriteReadRequest>,
    ) -> Result<u32, SbRmiCallError> {
        smm_helper32(rqst.body.command, Some(rqst.body.arg))
    }
}

#[cfg(feature = "unsafe-raw-commands")]
impl sp3_raw_sb_tsi::Server for ServerImpl {
    fn read_byte(&mut self, rqst: Request<'_, u8>) -> Result<u8, u32> {
        let task = I2C.get_task_id();
        let sbtsi = crate::i2c_config::devices::sbtsi_cpu(task);
        sbtsi.read_reg(*rqst.body).map_err(|code| code as u32)
    }

    fn write_byte(
        &mut self,
        rqst: Request<'_, SbTsiWriteRequest>,
    ) -> Result<(), u32> {
        let task = I2C.get_task_id();
        let sbtsi = crate::i2c_config::devices::sbtsi_cpu(task);
        sbtsi
            .write(&[rqst.body.reg, rqst.body.val])
            .map_err(|code| code as u32)
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    static BUFS: ClaimOnceCell<ApiBufs> = ClaimOnceCell::new(ApiBufs::new());

    let net = task_net_api::Net::from(NET.get_task_id());
    let socket =
        UdpSocket::new(net, SocketName::sbirpc, notifications::SOCKET_MASK);
    let mut backend = NetBackend::new(socket, BUFS.claim());
    let mut server = ServerImpl;

    backend.serve_forever(|b| composite::Server::serve_one(&mut server, b))
}

// Needed to get notification mask for net pingbacks
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
// Needed to get access to SB-TSI and SB-RMI interface names
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
