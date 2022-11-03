// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, Reg};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use drv_transceivers_api::ModulesStatus;
use zerocopy::{byteorder, AsBytes, FromBytes, Unaligned, U16};

pub struct Transceivers {
    fpgas: [FpgaUserDesign; 2],
}

// There are two FPGA controllers, each controlling the FPGA on either the left
// or right of the board.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum FpgaController {
    Left = 0,
    Right = 1,
}

// The necessary information to control a given port.
#[derive(Copy, Clone)]
struct PortLocation {
    controller: FpgaController,
    port: u8,
}

/// Port Map
///
/// Each index in this map represents the location of its transceiver port, so
/// index 0 is for port 0, and so on. The ports numbered 0-15 left to right
/// across the top of the board and 16-31 left to right across the bottom. The
/// ports are split up between the FPGAs based on locality, not logically and
/// the FPGAs share code, resulting in each one reporting in terms of ports 0-15
/// . This is the FPGA -> logical mapping.
const PORT_MAP: [PortLocation; 32] = [
    // Port 0
    PortLocation {
        controller: FpgaController::Left,
        port: 0,
    },
    // Port 1
    PortLocation {
        controller: FpgaController::Left,
        port: 1,
    },
    // Port 2
    PortLocation {
        controller: FpgaController::Left,
        port: 2,
    },
    // Port 3
    PortLocation {
        controller: FpgaController::Left,
        port: 3,
    },
    // Port 4
    PortLocation {
        controller: FpgaController::Left,
        port: 4,
    },
    // Port 5
    PortLocation {
        controller: FpgaController::Left,
        port: 5,
    },
    // Port 6
    PortLocation {
        controller: FpgaController::Left,
        port: 6,
    },
    // Port 7
    PortLocation {
        controller: FpgaController::Left,
        port: 7,
    },
    // Port 8
    PortLocation {
        controller: FpgaController::Right,
        port: 0,
    },
    // Port 9
    PortLocation {
        controller: FpgaController::Right,
        port: 1,
    },
    // Port 10
    PortLocation {
        controller: FpgaController::Right,
        port: 2,
    },
    // Port 11
    PortLocation {
        controller: FpgaController::Right,
        port: 3,
    },
    // Port 12
    PortLocation {
        controller: FpgaController::Right,
        port: 4,
    },
    // Port 13
    PortLocation {
        controller: FpgaController::Right,
        port: 5,
    },
    // Port 14
    PortLocation {
        controller: FpgaController::Right,
        port: 6,
    },
    // Port 15
    PortLocation {
        controller: FpgaController::Right,
        port: 7,
    },
    // Port 16
    PortLocation {
        controller: FpgaController::Left,
        port: 8,
    },
    // Port 17
    PortLocation {
        controller: FpgaController::Left,
        port: 9,
    },
    // Port 18
    PortLocation {
        controller: FpgaController::Left,
        port: 10,
    },
    // Port 19
    PortLocation {
        controller: FpgaController::Left,
        port: 11,
    },
    // Port 20
    PortLocation {
        controller: FpgaController::Left,
        port: 12,
    },
    // Port 21
    PortLocation {
        controller: FpgaController::Left,
        port: 13,
    },
    // Port 22
    PortLocation {
        controller: FpgaController::Left,
        port: 14,
    },
    // Port 23
    PortLocation {
        controller: FpgaController::Left,
        port: 15,
    },
    // Port 24
    PortLocation {
        controller: FpgaController::Right,
        port: 8,
    },
    // Port 25
    PortLocation {
        controller: FpgaController::Right,
        port: 9,
    },
    // Port 26
    PortLocation {
        controller: FpgaController::Right,
        port: 10,
    },
    // Port 27
    PortLocation {
        controller: FpgaController::Right,
        port: 11,
    },
    // Port 28
    PortLocation {
        controller: FpgaController::Right,
        port: 12,
    },
    // Port 29
    PortLocation {
        controller: FpgaController::Right,
        port: 13,
    },
    // Port 30
    PortLocation {
        controller: FpgaController::Right,
        port: 14,
    },
    // Port 31
    PortLocation {
        controller: FpgaController::Right,
        port: 15,
    },
];

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

    pub fn get_modules_status(&self) -> Result<ModulesStatus, FpgaError> {
        let f0: [U16<byteorder::BigEndian>; 7] =
            self.fpgas[0].read(Addr::QSFP_CTRL_EN_H)?;
        let f1: [U16<byteorder::BigEndian>; 7] =
            self.fpgas[1].read(Addr::QSFP_CTRL_EN_H)?;

        let mut status_masks: [u32; 7] = [0; 7];

        // loop through the 7 different fields we need to map
        for (i, mask) in status_masks.iter_mut().enumerate() {
            // loop through each port
            for port_loc in PORT_MAP {
                // get a mask for where the current logical port is mapped
                // locally on the FPGA
                let local_port_mask = 1 << port_loc.port;

                // get the relevant data from the correct FPGA
                let local_data: u16 = match port_loc.controller {
                    FpgaController::Left => f0[i],
                    FpgaController::Right => f1[i],
                }.into();

                // if the bit is set, update our status mask at the correct
                // logical position
                if (local_data & local_port_mask) != 0 {
                    let controller_offset = 16 * port_loc.controller as u32;
                    *mask |= (local_port_mask as u32) << controller_offset;
                }
            }
        }

        // for (data, (lo, hi)) in data.iter_mut().zip(f0.iter().zip(f1.iter())) {
        //     *data = (lo.get() as u32) | ((hi.get() as u32) << 16);
        // }

        Ok(ModulesStatus::read_from(status_masks.as_bytes()).unwrap())
    }

    pub fn masked_op(
        &self,
        op: WriteOp,
        mask: u32,
        addr: Addr,
    ) -> Result<(), FpgaError> {
        let mut fpga0_mask: u16 = 0;
        let mut fpga1_mask: u16 = 0;

        for (i, port_loc) in PORT_MAP.iter().enumerate() {
            let port_mask: u32 = 1 << i;
            if (mask & port_mask) != 0 {
                match port_loc.controller {
                    FpgaController::Left => fpga0_mask |= 1 << port_loc.port,
                    FpgaController::Right => fpga1_mask |= 1 << port_loc.port,
                }
            }
        }

        if fpga0_mask != 0 {
            let wdata: U16<byteorder::BigEndian> = U16::new(fpga0_mask);
            self.fpgas[0].write(op, addr, wdata)?;
        }
        if fpga1_mask != 0 {
            let wdata: U16<byteorder::BigEndian> = U16::new(fpga1_mask);
            self.fpgas[1].write(op, addr, wdata)?;
        }

        Ok(())
    }

    pub fn set_power_enable(&self, mask: u32) -> Result<(), FpgaError> {
        self.masked_op(WriteOp::BitSet, mask, Addr::QSFP_CTRL_EN_H)
    }

    pub fn clear_power_enable(&self, mask: u32) -> Result<(), FpgaError> {
        self.masked_op(WriteOp::BitClear, mask, Addr::QSFP_CTRL_EN_H)
    }

    pub fn set_reset(&self, mask: u32) -> Result<(), FpgaError> {
        self.masked_op(WriteOp::BitSet, mask, Addr::QSFP_CTRL_RESET_H)
    }

    pub fn clear_reset(&self, mask: u32) -> Result<(), FpgaError> {
        self.masked_op(WriteOp::BitClear, mask, Addr::QSFP_CTRL_RESET_H)
    }

    pub fn set_lpmode(&self, mask: u32) -> Result<(), FpgaError> {
        self.masked_op(WriteOp::BitSet, mask, Addr::QSFP_CTRL_LPMODE_H)
    }

    pub fn clear_lpmode(&self, mask: u32) -> Result<(), FpgaError> {
        self.masked_op(WriteOp::BitClear, mask, Addr::QSFP_CTRL_LPMODE_H)
    }

    pub fn setup_i2c_op(
        &self,
        is_read: bool,
        reg: u8,
        num_bytes: u8,
        mask: u32,
    ) -> Result<(), FpgaError> {
        let fpga0_mask = (mask & 0xFFFF) as u16;
        let fpga1_mask = ((mask & 0xFFFF0000) >> 16) as u16;

        let i2c_op = if is_read {
            // Defaulting to RandomRead, rather than Read, because RandomRead
            // sets the reg addr in the target device, then issues a restart to
            // do the read at that reg addr. On the other hand, Read just starts
            // a read wherever the reg addr is after the last transaction.
            TransceiverI2COperation::RandomRead
        } else {
            TransceiverI2COperation::Write
        };

        if fpga0_mask != 0 {
            let request = TransceiversI2CRequest {
                reg,
                num_bytes,
                mask: U16::new(fpga0_mask),
                op: i2c_op as u8,
            };

            self.fpgas[0].write(
                WriteOp::Write,
                Addr::QSFP_I2C_REG_ADDR,
                request,
            )?;
        }

        if fpga1_mask != 0 {
            let request = TransceiversI2CRequest {
                reg,
                num_bytes,
                mask: U16::new(fpga1_mask),
                op: i2c_op as u8,
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
        let fpga_idx: usize = if port < 16 { 0 } else { 1 };
        self.fpgas[fpga_idx].read_bytes(Self::read_buffer_address(port), buf)
    }

    // Setting data in the write buffer does not require a port be specified.
    // This is because in the FPGA implementation, the write buffer being
    // written to simply pushes a copy of the data into each individual port's
    // write buffer. This keeps us from needing to write to them all
    // individually.
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
            _ => unreachable!(),
        }
    }

    /// Releases the LED controller from reset and enables the output
    pub fn enable_led_controllers(&mut self) -> Result<(), FpgaError> {
        for fpga in &self.fpgas {
            fpga.write(
                WriteOp::BitClear,
                Addr::LED_CTRL,
                Reg::LED_CTRL::RESET,
            )?;
        }

        Ok(())
    }
}

// The I2C control register looks like:
// [2..1] - Operation (0 - Read, 1 - Write, 2 - RandomRead)
// [0] - Start
#[derive(AsBytes)]
#[repr(u8)]
pub enum TransceiverI2COperation {
    Read = 0x01,
    Write = 0x03,
    // Start a Write to set the reg addr, then Start again to do read at that addr
    RandomRead = 0x05,
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
    mask: U16<byteorder::BigEndian>,
    op: u8,
}
