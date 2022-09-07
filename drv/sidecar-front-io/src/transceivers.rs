// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, Reg};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use zerocopy::{AsBytes, FromBytes, Unaligned};

pub struct Transceivers {
    fpgas: [FpgaUserDesign; 2],
}

impl Transceivers {
    pub fn new(fpga_task: userlib::TaskId) -> Self {
        Self {
            // There are 16 QSFP-DD transceivers connected to each FPGA
            fpgas: [
                FpgaUserDesign::new(fpga_task, 0),
                FpgaUserDesign::new(fpga_task, 1),
            ],
        }
    }

    pub fn get_modules_status(&self) -> Result<[u32; 7], FpgaError> {
        let mut r: [u32; 7] = [0; 7];
        let f0: [u8; 14] = self.fpgas[0].read(Addr::QSFP_CTRL_EN_H)?;
        let f1: [u8; 14] = self.fpgas[1].read(Addr::QSFP_CTRL_EN_H)?;

        for i in 0..7 {
            r[i] = ((f1[i * 2] as u32) << 24)
                | ((f1[i * 2 + 1] as u32) << 16)
                | ((f0[i * 2] as u32) << 8)
                | (f0[i * 2 + 1] as u32);
        }

        Ok(r)
    }

    pub fn set_power_enable(&self, mask: u32) -> Result<(), FpgaError> {
        let m = mask.as_bytes();

        if (mask & 0xFFFF) > 0 {
            self.fpgas[0].write(
                WriteOp::BitSet,
                Addr::QSFP_CTRL_EN_H,
                [m[1], m[0]],
            )?;
        }
        if (mask & 0xFFFF0000) > 0 {
            self.fpgas[1].write(
                WriteOp::BitSet,
                Addr::QSFP_CTRL_EN_H,
                [m[3], m[2]],
            )?;
        }

        Ok(())
    }

    pub fn clear_power_enable(&self, mask: u32) -> Result<(), FpgaError> {
        let m = mask.as_bytes();

        if (mask & 0xFFFF) > 0 {
            self.fpgas[0].write(
                WriteOp::BitClear,
                Addr::QSFP_CTRL_EN_H,
                [m[1], m[0]],
            )?;
        }
        if (mask & 0xFFFF0000) > 0 {
            self.fpgas[1].write(
                WriteOp::BitClear,
                Addr::QSFP_CTRL_EN_H,
                [m[3], m[2]],
            )?;
        }

        Ok(())
    }

    pub fn set_reset(&self, mask: u32) -> Result<(), FpgaError> {
        let m = mask.as_bytes();

        if (mask & 0xFFFF) > 0 {
            self.fpgas[0].write(
                WriteOp::BitSet,
                Addr::QSFP_CTRL_RESET_H,
                [m[1], m[0]],
            )?;
        }
        if (mask & 0xFFFF0000) > 0 {
            self.fpgas[1].write(
                WriteOp::BitSet,
                Addr::QSFP_CTRL_RESET_H,
                [m[3], m[2]],
            )?;
        }

        Ok(())
    }

    pub fn clear_reset(&self, mask: u32) -> Result<(), FpgaError> {
        let m = mask.as_bytes();

        if (mask & 0xFFFF) > 0 {
            self.fpgas[0].write(
                WriteOp::BitClear,
                Addr::QSFP_CTRL_RESET_H,
                [m[1], m[0]],
            )?;
        }
        if (mask & 0xFFFF0000) > 0 {
            self.fpgas[1].write(
                WriteOp::BitClear,
                Addr::QSFP_CTRL_RESET_H,
                [m[3], m[2]],
            )?;
        }

        Ok(())
    }

    pub fn setup_i2c_op(
        &self,
        is_read: bool,
        reg: u8,
        num_bytes: u8,
        mask: u32,
    ) -> Result<(), FpgaError> {
        let m = mask.as_bytes();

        let i2c_op = if is_read {
            // Defaulting to RandomRead, rather than Read, because RandomRead
            // sets the reg addr in the target device, then issues a restart to
            // do the read at that reg addr. On the other hand, Read just starts
            // a read wherever the reg addr is after the last transaction.
            TransceiverI2COperation::RandomRead
        } else {
            TransceiverI2COperation::Write
        };

        if (mask & 0xFFFF) > 0 {
            let request = TransceiversI2CRequest {
                reg,
                num_bytes,
                mask: [m[1], m[0]],
                op: ((i2c_op as u8) << 1) | Reg::QSFP::I2C_CTRL::START,
            };

            self.fpgas[0].write(
                WriteOp::Write,
                Addr::QSFP_I2C_REG_ADDR,
                request,
            )?;
        }

        if (mask & 0xFFFF0000) > 0 {
            let request = TransceiversI2CRequest {
                reg,
                num_bytes,
                mask: [m[3], m[2]],
                op: ((i2c_op as u8) << 1) | Reg::QSFP::I2C_CTRL::START,
            };
            self.fpgas[1].write(
                WriteOp::Write,
                Addr::QSFP_I2C_REG_ADDR,
                request,
            )?;
        }

        Ok(())
    }

    pub fn get_i2c_read_buffer(
        &self,
        port: u8,
        buf: &mut [u8],
    ) -> Result<(), FpgaError> {
        let fpga_idx: usize = if port < 16 as u8 { 0 } else { 1 };
        self.fpgas[fpga_idx].read_bytes(Self::read_buffer_address(port), buf)
    }

    pub fn set_i2c_write_buffer(&self, buf: &[u8]) -> Result<(), FpgaError> {
        self.fpgas[0].write_bytes(
            WriteOp::Write,
            Addr::QSFP_WRITE_BUFFER,
            buf,
        )?;
        self.fpgas[1].write_bytes(
            WriteOp::Write,
            Addr::QSFP_WRITE_BUFFER,
            buf,
        )?;

        Ok(())
    }

    pub fn read_buffer_address(port: u8) -> Addr {
        match port % 16 {
            0 => Addr::QSFP_PORT0_READ_BUFFER,
            1 => Addr::QSFP_PORT1_READ_BUFFER,
            2 => Addr::QSFP_PORT2_READ_BUFFER,
            3 => Addr::QSFP_PORT3_READ_BUFFER,
            4 => Addr::QSFP_PORT4_READ_BUFFER,
            5 => Addr::QSFP_PORT5_READ_BUFFER,
            6 => Addr::QSFP_PORT6_READ_BUFFER,
            7 => Addr::QSFP_PORT7_READ_BUFFER,
            8 => Addr::QSFP_PORT8_READ_BUFFER,
            9 => Addr::QSFP_PORT9_READ_BUFFER,
            10 => Addr::QSFP_PORT10_READ_BUFFER,
            11 => Addr::QSFP_PORT11_READ_BUFFER,
            12 => Addr::QSFP_PORT12_READ_BUFFER,
            13 => Addr::QSFP_PORT13_READ_BUFFER,
            14 => Addr::QSFP_PORT14_READ_BUFFER,
            15 => Addr::QSFP_PORT15_READ_BUFFER,
            _ => Addr::QSFP_PORT0_READ_BUFFER,
        }
    }
}

#[derive(AsBytes)]
#[repr(u8)]
pub enum TransceiverI2COperation {
    Read = 0,
    Write = 1,
    // Start a Write to set the reg addr, then Start again to do read at that addr
    RandomRead = 2,
}

impl From<TransceiverI2COperation> for u8 {
    fn from(op: TransceiverI2COperation) -> Self {
        op as u8
    }
}

#[derive(AsBytes, FromBytes, Unaligned)]
#[repr(C)]
pub struct TransceiversI2CRequest {
    reg: u8,
    num_bytes: u8,
    mask: [u8; 2],
    op: u8,
}
