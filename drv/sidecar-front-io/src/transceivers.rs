// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, Reg};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use drv_transceivers_api::{ModulesStatus, PowerState, PowerStatesAll};
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
pub struct PortLocation {
    pub controller: FpgaController,
    pub port: u8,
}

/// Physical port maps, using bitfields to mark active ports
#[derive(Copy, Clone)]
pub struct FpgaPortMasks {
    pub left: u16,
    pub right: u16,
}

impl FpgaPortMasks {
    /// Returns an iterator over FPGAs that are active in the mask
    ///
    /// (possibilities include `Left`, `Right`, both, or none)
    fn iter_fpgas(&self) -> impl Iterator<Item = FpgaController> {
        let out = [
            Some(FpgaController::Left).filter(|_| self.left != 0),
            Some(FpgaController::Right).filter(|_| self.right != 0),
        ];
        out.into_iter().flatten()
    }
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

// Maps logical port `mask` to physical FPGA locations
impl From<u32> for FpgaPortMasks {
    fn from(mask: u32) -> FpgaPortMasks {
        let mut fpga_port_masks = FpgaPortMasks { left: 0, right: 0 };

        for (i, port_loc) in PORT_MAP.iter().enumerate() {
            let port_mask: u32 = 1 << i;
            if (mask & port_mask) != 0 {
                match port_loc.controller {
                    FpgaController::Left => {
                        fpga_port_masks.left |= 1 << port_loc.port
                    }
                    FpgaController::Right => {
                        fpga_port_masks.right |= 1 << port_loc.port
                    }
                }
            }
        }

        fpga_port_masks
    }
}

impl From<u8> for PortLocation {
    fn from(port: u8) -> PortLocation {
        PORT_MAP[port as usize]
    }
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

    pub fn fpga(&self, c: FpgaController) -> &FpgaUserDesign {
        &self.fpgas[c as usize]
    }

    // helper to write a value to CONTROL_PORTx registers given a mask
    fn write_control_port<M: Into<FpgaPortMasks>>(
        &self,
        mask: M,
        value: u8,
    ) -> Result<(), FpgaError> {
        let fpga_masks: FpgaPortMasks = mask.into();
        for fpga_index in fpga_masks.iter_fpgas() {
            let mask = match fpga_index {
                FpgaController::Left => fpga_masks.left,
                FpgaController::Right => fpga_masks.right,
            };

            if mask != 0 {
                let fpga = self.fpga(fpga_index);
                for port in 0..16 {
                    if mask & (1 << port) != 0 {
                        fpga.write(
                            WriteOp::Write,
                            Addr::QSFP_CONTROL_PORT0 as u16 + port,
                            value,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Get the current status of all low speed signals for all ports. This is
    /// Enable, Reset, LpMode/TxDis, Power Good, Power Good Timeout, Present,
    /// and IRQ/RxLos.
    pub fn get_modules_status(&self) -> Result<ModulesStatus, FpgaError> {
        let f0: [U16<byteorder::BigEndian>; 8] =
            self.fpga(FpgaController::Left).read(Addr::QSFP_CTRL_EN_H)?;
        let f1: [U16<byteorder::BigEndian>; 8] = self
            .fpga(FpgaController::Right)
            .read(Addr::QSFP_CTRL_EN_H)?;

        let mut status_masks: [u32; 8] = [0; 8];

        // loop through each port
        for (port, port_loc) in PORT_MAP.iter().enumerate() {
            // get a mask for where the current logical port is mapped
            // locally on the FPGA
            let local_port_mask = 1 << port_loc.port;

            // get the relevant data from the correct FPGA
            let local_data = match port_loc.controller {
                FpgaController::Left => &f0,
                FpgaController::Right => &f1,
            };

            // loop through the 8 different fields we need to map
            for (word, out) in local_data.iter().zip(status_masks.iter_mut()) {
                // if the bit is set, update our status mask at the correct
                // logical position
                let word: u16 = (*word).into();
                if (word & local_port_mask) != 0 {
                    *out |= 1 << port;
                }
            }
        }

        Ok(ModulesStatus::read_from(status_masks.as_bytes()).unwrap())
    }

    /// Get the power state of all modules
    pub fn all_power_states(&self) -> Result<PowerStatesAll, FpgaError> {
        let f0: [u8; 16] = self
            .fpga(FpgaController::Left)
            .read(Addr::QSFP_STATUS_PORT0)?;
        let f1: [u8; 16] = self
            .fpga(FpgaController::Right)
            .read(Addr::QSFP_STATUS_PORT0)?;

        let mut data: [PowerState; 32] = [PowerState::A4; 32];

        // loop through each port
        for (port, port_loc) in PORT_MAP.iter().enumerate() {
            // get the relevant data from the correct FPGA
            let local_data = match port_loc.controller {
                FpgaController::Left => &f0,
                FpgaController::Right => &f1,
            };
            let power_status_field = (local_data[port_loc.port as usize]
                & Reg::QSFP::CONTROL_PORT0::POWER_STATE)
                >> 5;
            data[port] = power_status_field
                .try_into()
                .map_err(|_| FpgaError::InvalidValue)?;
        }

        Ok(PowerStatesAll::new(data))
    }

    /// Set the power state to `state` for all ports per the specified `mask`.
    pub fn set_power_state<M: Into<FpgaPortMasks>>(
        &self,
        state: PowerState,
        mask: M,
    ) -> Result<(), FpgaError> {
        // POWER_STATE field in CONTROL_PORTx register is bits 7..5
        let new_state = (state as u8) << 5;
        self.write_control_port(mask, new_state)
    }

    // Set the power state to A3 for all ports per the specified `mask`
    pub fn port_off<M: Into<FpgaPortMasks>>(
        &self,
        mask: M,
    ) -> Result<(), FpgaError> {
        self.set_power_state(PowerState::A3, mask)
    }

    // Set the power state to A2 for all ports per the specified `mask`
    pub fn port_low_power<M: Into<FpgaPortMasks>>(
        &self,
        mask: M,
    ) -> Result<(), FpgaError> {
        self.set_power_state(PowerState::A2, mask)
    }

    // Set the power state to A0 for all ports per the specified `mask`
    pub fn port_high_power<M: Into<FpgaPortMasks>>(
        &self,
        mask: M,
    ) -> Result<(), FpgaError> {
        self.set_power_state(PowerState::A0, mask)
    }

    /// Request a reset for each port per the specified `mask` and leave that
    /// port in A2 (LowPower).
    pub fn port_reset<M: Into<FpgaPortMasks>>(
        &self,
        mask: M,
    ) -> Result<(), FpgaError> {
        // POWER_STATE field in CONTROL_PORTx register is bits 7..5
        let new_state =
            (PowerState::A2 as u8) << 5 | Reg::QSFP::CONTROL_PORT0::RESET;
        self.write_control_port(mask, new_state)
    }

    /// Clear a fault for each port per the specified `mask` and leave that port
    /// in A3 (Off).
    pub fn port_clear_fault<M: Into<FpgaPortMasks>>(
        &self,
        mask: M,
    ) -> Result<(), FpgaError> {
        // POWER_STATE field in CONTROL_PORTx register is bits 7..5
        let new_state =
            (PowerState::A3 as u8) << 5 | Reg::QSFP::CONTROL_PORT0::CLEAR_FAULT;
        self.write_control_port(mask, new_state)
    }

    /// Initiate an I2C random read on all ports per the specified `mask`.
    ///
    /// The maximum value of `num_bytes` is 128.
    pub fn setup_i2c_read<M: Into<FpgaPortMasks>>(
        &self,
        reg: u8,
        num_bytes: u8,
        mask: M,
    ) -> Result<(), FpgaError> {
        self.setup_i2c_op(true, reg, num_bytes, mask)
    }

    /// Initiate an I2C write on all ports per the specified `mask`.
    ///
    /// The maximum value of `num_bytes` is 128.
    pub fn setup_i2c_write<M: Into<FpgaPortMasks>>(
        &self,
        reg: u8,
        num_bytes: u8,
        mask: M,
    ) -> Result<(), FpgaError> {
        self.setup_i2c_op(false, reg, num_bytes, mask)
    }

    /// Initiate an I2C operation on all ports per the specified `mask`. When
    /// `is_read` is true, the operation will be a random-read, not a pure I2C
    /// read. The maximum value of `num_bytes` is 128.
    fn setup_i2c_op<M: Into<FpgaPortMasks>>(
        &self,
        is_read: bool,
        reg: u8,
        num_bytes: u8,
        mask: M,
    ) -> Result<(), FpgaError> {
        let fpga_masks: FpgaPortMasks = mask.into();

        let i2c_op = if is_read {
            // Defaulting to RandomRead, rather than Read, because RandomRead
            // sets the reg addr in the target device, then issues a restart to
            // do the read at that reg addr. On the other hand, Read just starts
            // a read wherever the reg addr is after the last transaction.
            TransceiverI2COperation::RandomRead
        } else {
            TransceiverI2COperation::Write
        };

        if fpga_masks.left != 0 {
            let request = TransceiversI2CRequest {
                reg,
                num_bytes,
                mask: U16::new(fpga_masks.left),
                op: i2c_op as u8,
            };

            self.fpga(FpgaController::Left).write(
                WriteOp::Write,
                Addr::QSFP_I2C_REG_ADDR,
                request,
            )?;
        }

        if fpga_masks.right != 0 {
            let request = TransceiversI2CRequest {
                reg,
                num_bytes,
                mask: U16::new(fpga_masks.right),
                op: i2c_op as u8,
            };
            self.fpga(FpgaController::Right).write(
                WriteOp::Write,
                Addr::QSFP_I2C_REG_ADDR,
                request,
            )?;
        }

        Ok(())
    }

    /// Get `buf.len()` bytes of data from the I2C read buffer for a `port`. The
    /// buffer stores data from the last I2C read transaction done and thus only
    /// the number of bytes read will be valid in the buffer.
    pub fn get_i2c_read_buffer<P: Into<PortLocation>>(
        &self,
        port: P,
        buf: &mut [u8],
    ) -> Result<(), FpgaError> {
        let port_loc = port.into();
        self.fpga(port_loc.controller)
            .read_bytes(Self::read_buffer_address(port_loc.port), buf)
    }

    /// Write `buf.len()` bytes of data into the I2C write buffer. Upon a write
    /// transaction happening, the number of bytes specified will be pulled from
    /// the write buffer. Setting data in the write buffer does not require a
    /// port be specified. This is because in the FPGA implementation, the write
    /// buffer being written to simply pushes a copy of the data into each
    /// individual port's write buffer. This keeps us from needing to write to
    /// them all individually.
    pub fn set_i2c_write_buffer(&self, buf: &[u8]) -> Result<(), FpgaError> {
        self.fpga(FpgaController::Left).write_bytes(
            WriteOp::Write,
            Addr::QSFP_WRITE_BUFFER,
            buf,
        )?;
        self.fpga(FpgaController::Right).write_bytes(
            WriteOp::Write,
            Addr::QSFP_WRITE_BUFFER,
            buf,
        )?;

        Ok(())
    }

    /// For a given `local_port`, return the Addr where its read buffer begins
    pub fn read_buffer_address(local_port: u8) -> Addr {
        match local_port % 16 {
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

    pub fn read_status_address(local_port: u8) -> Addr {
        match local_port % 16 {
            0 => Addr::QSFP_PORT0_STATUS,
            1 => Addr::QSFP_PORT1_STATUS,
            2 => Addr::QSFP_PORT2_STATUS,
            3 => Addr::QSFP_PORT3_STATUS,
            4 => Addr::QSFP_PORT4_STATUS,
            5 => Addr::QSFP_PORT5_STATUS,
            6 => Addr::QSFP_PORT6_STATUS,
            7 => Addr::QSFP_PORT7_STATUS,
            8 => Addr::QSFP_PORT8_STATUS,
            9 => Addr::QSFP_PORT9_STATUS,
            10 => Addr::QSFP_PORT10_STATUS,
            11 => Addr::QSFP_PORT11_STATUS,
            12 => Addr::QSFP_PORT12_STATUS,
            13 => Addr::QSFP_PORT13_STATUS,
            14 => Addr::QSFP_PORT14_STATUS,
            15 => Addr::QSFP_PORT15_STATUS,
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

    /// Waits for all of the I2C busy bits to go low
    ///
    /// Returns a set of masks indicating which channels (among the ones active
    /// in the input mask) have recorded FPGA errors.
    pub fn wait_and_check_i2c(
        &mut self,
        mask: FpgaPortMasks,
    ) -> Result<FpgaPortMasks, FpgaError> {
        let mut out = FpgaPortMasks { left: 0, right: 0 };

        #[derive(AsBytes, Default, FromBytes)]
        #[repr(C)]
        struct StatusAndErr {
            busy: u16,
            err: [u8; 8],
        }
        for fpga_index in mask.iter_fpgas() {
            let fpga = self.fpga(fpga_index);
            // This loop should break immediately, because I2C is fast
            let status = loop {
                // Two bytes of BUSY, followed by 8 bytes of error status
                let status: StatusAndErr = fpga.read(Addr::QSFP_I2C_BUSY_H)?;
                if status.busy == 0 {
                    break status;
                }
                userlib::hl::sleep_for(1);
            };

            // Check errors on a per-port basis
            let mask = match fpga_index {
                FpgaController::Left => mask.left,
                FpgaController::Right => mask.right,
            };
            for port in 0..16 {
                if mask & (1 << port) == 0 {
                    continue;
                }
                // Each error byte packs together two ports
                let err = status.err[port / 2] >> ((port % 2) * 4);

                // For now, check for the presence of an error, but don't bother
                // reporting the details.
                let has_err = (err & 0b1000) != 0;
                if has_err {
                    match fpga_index {
                        FpgaController::Left => out.left |= 1 << port,
                        FpgaController::Right => out.right |= 1 << port,
                    }
                }
            }
        }
        Ok(out)
    }
}

// The I2C control register looks like:
// [2..1] - Operation (0 - Read, 1 - Write, 2 - RandomRead)
// [0] - Start
#[derive(Copy, Clone, Debug, AsBytes)]
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
