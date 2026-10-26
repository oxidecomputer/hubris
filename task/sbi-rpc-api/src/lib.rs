// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API type definitions for `task/sbi-rpc`.

#![cfg_attr(not(feature = "std"), no_std)]

use nprpc::interface;
use postcard_schema_ng::Schema;
pub use postcard_schema_ng::max_len::MaxLenByteSlice;
use serde::{Deserialize, Serialize};

/// Request for SB-RMI Mailbox calls that take a `u32` arg.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Schema)]
pub struct SbRmiCallWriteReadRequest {
    /// Command (written to `InBndMsg_inst0`).
    pub command: u8,
    /// Argument (written to `InBndMsg_inst[4:1]`).
    pub arg: u32,
}

/// Error while making an SB-RMI Mailbox call
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Schema)]
pub enum SbRmiCallError {
    /// Error from the i2c-server.
    I2cError { code: u32 },
    /// Exceeded timeout while waiting for command to complete.
    Timeout,
    /// Command completed, but returned the "Command Aborted" error code.
    CmdAborted { payload: u32 },
    /// Command completed, but returned the "Unknown Command" error code.
    CmdUnknown { payload: u32 },
    /// Command completed, but returned the "Invalid Core" error code.
    CmdBadCore { payload: u32 },
    /// Command completed, but returned an un-enumerated error code.
    UnexpectedStatus { payload: u32, code: u8 },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Schema)]
pub struct SbTsiWriteRequest {
    /// Register address.
    pub reg: u8,
    /// Register contents to write.
    pub val: u8,
}

interface! {
    /// Server interface for raw SB-RMI commands.
    mod sp3_raw_sb_rmi {
        /// SB-RMI Mailbox Message that takes no argument and returns a single
        /// 32-bit word.
        fn smm_read_word(u8) -> Result<u32, SbRmiCallError>;
        /// SB-RMI Mailbox Message that takes a 32-bit argument and returns a
        /// single 32-bit word.
        fn smm_write_read_word(SbRmiCallWriteReadRequest)
            -> Result<u32, SbRmiCallError>;
    }
}

interface! {
    /// Server interface for raw SB-TSI commands.
    mod sp3_raw_sb_tsi {
        /// Read a u8 value from the given u8 register.
        fn read_byte(u8) -> Result<u8, u32>;
        /// Write a given u8 value to a given u8 register.
        fn write_byte(SbTsiWriteRequest) -> Result<(), u32>;
    }
}

interface! {
    /// Allow-listed interfaces for SB-RMI consumption
    mod sp3_sb_rmi {
        fn read_package_power_consumption(()) -> Result<u32, SbRmiCallError>;
        fn read_package_power_limit(()) -> Result<u32, SbRmiCallError>;
        fn read_max_package_power_limit(()) -> Result<u32, SbRmiCallError>;
        fn read_tdp(()) -> Result<u32, SbRmiCallError>;
        fn read_max_ctdp(()) -> Result<u32, SbRmiCallError>;
        fn read_min_ctdp(()) -> Result<u32, SbRmiCallError>;
        fn read_bios_boost_fmax(u16) -> Result<u32, SbRmiCallError>;
        fn read_apml_boost_limit(u16) -> Result<u32, SbRmiCallError>;
        fn read_dram_throttle(()) -> Result<u32, SbRmiCallError>;
        fn read_prochot_status(()) -> Result<u32, SbRmiCallError>;
        fn read_prochot_residency(()) -> Result<u32, SbRmiCallError>;
        fn read_vddio_mem_power(()) -> Result<u32, SbRmiCallError>;
        fn read_nbio_error_logging(u32) -> Result<u32, SbRmiCallError>;
        fn read_iod_bist_result(()) -> Result<u32, SbRmiCallError>;
        fn read_ccd_bist_result(u32) -> Result<u32, SbRmiCallError>;
        fn read_ccx_bist_result(u32) -> Result<u32, SbRmiCallError>;
        fn read_cclk_freq_limit(()) -> Result<u32, SbRmiCallError>;
        fn read_socket_c0_residency(()) -> Result<u32, SbRmiCallError>;
        fn read_max_ddr_bw_util(()) -> Result<u32, SbRmiCallError>;
    }
}

interface! {
    /// Allow-listed interfaces for SB-TSI consumption
    ///
    /// The `Result::Err(u32)` here represents a raw i2c-server response code.
    mod sp3_sb_tsi {
        fn read_cpu_temp(()) -> Result<f32, u32>;
        fn read_status(()) -> Result<u8, u32>;
        fn read_config(()) -> Result<u8, u32>;
        fn read_update_rate(()) -> Result<u8, u32>;
        fn read_high_temp_threshold(()) -> Result<f32, u32>;
        fn read_low_temp_threshold(()) -> Result<f32, u32>;
        fn read_timeout_config(()) -> Result<u8, u32>;
        fn read_alert_threshold(()) -> Result<u8, u32>;
        fn read_alert_config(()) -> Result<u8, u32>;
        fn read_manufacture_id(()) -> Result<u8, u32>;
        fn read_revision(()) -> Result<u8, u32>;
    }
}
