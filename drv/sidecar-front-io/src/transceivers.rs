// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, Reg};
use drv_fpga_api::{FpgaError, FpgaUserDesign, ReadOp, WriteOp};
use drv_transceivers_api::{ModuleStatus, TransceiversError, NUM_PORTS};
use transceiver_messages::ModuleId;
use userlib::UnwrapLite;
use zerocopy::{byteorder, FromBytes, IntoBytes, Unaligned, U16};

// The transceiver modules are split across two FPGAs on the QSFP Front IO
// board, so while we present the modules as a unit, the communication is
// actually bifurcated.
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

/// Physical port location
#[derive(Copy, Clone, PartialEq)]
pub struct PortLocation {
    pub controller: FpgaController,
    pub port: PhysicalPort,
}

impl From<LogicalPort> for PortLocation {
    fn from(port: LogicalPort) -> PortLocation {
        PORT_MAP[port]
    }
}

/// Physical port location within a particular FPGA, as a 0-15 index
#[derive(Copy, Clone, PartialEq)]
pub struct PhysicalPort(pub u8);
impl PhysicalPort {
    pub fn as_mask(&self) -> PhysicalPortMask {
        PhysicalPortMask(1 << self.0)
    }

    pub fn get(&self) -> u8 {
        self.0
    }

    pub fn to_logical_port(
        &self,
        fpga: FpgaController,
    ) -> Result<LogicalPort, TransceiversError> {
        let loc = PortLocation {
            controller: fpga,
            port: *self,
        };
        match PORT_MAP.into_iter().position(|&l| l == loc) {
            Some(p) => Ok(LogicalPort(p as u8)),
            None => Err(TransceiversError::InvalidPhysicalToLogicalMap),
        }
    }
}

/// Physical port mask within a particular FPGA, as a 16-bit bitfield
#[derive(Copy, Clone, Default)]
pub struct PhysicalPortMask(pub u16);
impl PhysicalPortMask {
    pub fn get(&self) -> u16 {
        self.0
    }
    pub fn set(&mut self, index: PhysicalPort) {
        self.0 |= index.as_mask().0
    }
    pub fn merge(&mut self, other: PhysicalPortMask) {
        self.0 |= other.0
    }
    pub fn is_set(&self, index: PhysicalPort) -> bool {
        self.0 & index.as_mask().0 != 0
    }
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

/// Physical port maps, using bitfields to mark active ports
#[derive(Copy, Clone, Default)]
pub struct FpgaPortMasks {
    pub left: PhysicalPortMask,
    pub right: PhysicalPortMask,
}

impl FpgaPortMasks {
    /// Returns an iterator over FPGAs that are active in the mask
    ///
    /// (possibilities include `Left`, `Right`, both, or none)
    fn iter_fpgas(&self) -> impl Iterator<Item = FpgaController> {
        let out = [
            Some(FpgaController::Left).filter(|_| !self.left.is_empty()),
            Some(FpgaController::Right).filter(|_| !self.right.is_empty()),
        ];
        out.into_iter().flatten()
    }

    fn get(&self, fpga: FpgaController) -> PhysicalPortMask {
        match fpga {
            FpgaController::Left => self.left,
            FpgaController::Right => self.right,
        }
    }

    fn get_mut(&mut self, fpga: FpgaController) -> &mut PhysicalPortMask {
        match fpga {
            FpgaController::Left => &mut self.left,
            FpgaController::Right => &mut self.right,
        }
    }
}

/// Represents a single logical port (0-31)
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd)]
pub struct LogicalPort(pub u8);
impl LogicalPort {
    pub fn as_mask(&self) -> LogicalPortMask {
        LogicalPortMask(1 << self.0)
    }

    pub fn get_physical_location(&self) -> PortLocation {
        PortLocation::from(*self)
    }
}
/// Represents a set of selected logical ports, i.e. a 32-bit bitmask
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LogicalPortMask(pub u32);

impl LogicalPortMask {
    pub const MAX_PORT_INDEX: LogicalPort = LogicalPort(NUM_PORTS - 1);

    pub fn get(&self) -> u32 {
        self.0
    }
    pub fn set(&mut self, index: LogicalPort) {
        *self |= index
    }
    pub fn clear(&mut self, index: LogicalPort) {
        *self &= !index.as_mask()
    }
    pub fn merge(&mut self, other: LogicalPortMask) {
        *self |= other
    }
    pub fn count(&self) -> usize {
        self.0.count_ones() as _
    }
    pub fn is_set(&self, index: LogicalPort) -> bool {
        !(*self & index).is_empty()
    }
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
    pub fn to_indices(&self) -> impl Iterator<Item = LogicalPort> + '_ {
        (0..32).map(LogicalPort).filter(|p| self.is_set(*p))
    }
}

// `ModuleId` is a 64-bit logical port mask. The choice of u64 was to provide
// future flexibility, but currently we only support 32 distinct ports, so we
// ignore the upper 32 bits of `ModuleId` when constructing a `LogicalPortMask`.
impl From<ModuleId> for LogicalPortMask {
    fn from(v: ModuleId) -> Self {
        Self(v.0 as u32)
    }
}

impl From<LogicalPortMask> for ModuleId {
    fn from(v: LogicalPortMask) -> Self {
        Self(v.0 as u64)
    }
}

// It is convenient to have the ergonomics for a LogicalPortMask resemble the
// bitwise mask that it represents, so we implement some bitwise operations.
impl core::ops::BitOr for LogicalPortMask {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        LogicalPortMask(self.0 | rhs.0)
    }
}

impl core::ops::BitOr<LogicalPort> for LogicalPortMask {
    type Output = Self;

    fn bitor(self, rhs: LogicalPort) -> Self {
        LogicalPortMask(self.0 | rhs.as_mask().0)
    }
}

impl core::ops::BitOrAssign for LogicalPortMask {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = *self | rhs
    }
}

impl core::ops::BitOrAssign<LogicalPort> for LogicalPortMask {
    fn bitor_assign(&mut self, rhs: LogicalPort) {
        *self = *self | rhs
    }
}

impl core::ops::BitAnd for LogicalPortMask {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        LogicalPortMask(self.0 & rhs.0)
    }
}

impl core::ops::BitAnd<LogicalPort> for LogicalPortMask {
    type Output = Self;

    fn bitand(self, rhs: LogicalPort) -> Self {
        LogicalPortMask(self.0 & rhs.as_mask().0)
    }
}

impl core::ops::BitAndAssign for LogicalPortMask {
    fn bitand_assign(&mut self, rhs: Self) {
        *self = *self & rhs
    }
}

impl core::ops::BitAndAssign<LogicalPort> for LogicalPortMask {
    fn bitand_assign(&mut self, rhs: LogicalPort) {
        *self = *self & rhs
    }
}

impl core::ops::Not for LogicalPortMask {
    type Output = Self;

    fn not(self) -> Self {
        LogicalPortMask(!self.0)
    }
}

// Maps physical port `mask` to a logical port mask
impl From<FpgaPortMasks> for LogicalPortMask {
    fn from(mask: FpgaPortMasks) -> LogicalPortMask {
        let mut logical_mask = LogicalPortMask(0);
        for logical_port in (0..NUM_PORTS).map(LogicalPort) {
            let port_location = PortLocation::from(logical_port);
            let bits = mask.get(port_location.controller);
            if bits.is_set(port_location.port) {
                logical_mask |= logical_port;
            }
        }
        logical_mask
    }
}

// Maps logical port `mask` to physical FPGA locations
impl From<LogicalPortMask> for FpgaPortMasks {
    fn from(mask: LogicalPortMask) -> FpgaPortMasks {
        let mut fpga_port_masks = FpgaPortMasks::default();
        for (i, port_loc) in PORT_MAP.enumerate() {
            if mask.is_set(i) {
                fpga_port_masks
                    .get_mut(port_loc.controller)
                    .set(port_loc.port);
            }
        }
        fpga_port_masks
    }
}

/// Logical -> physical mapping for ports
///
/// Each index in this map represents the location of its transceiver port, so
/// index 0 is for port 0, and so on. The ports numbered 0-15 left to right
/// across the top of the board and 16-31 left to right across the bottom. The
/// ports are split up between the FPGAs based on locality, not logically and
/// the FPGAs share code, resulting in each one reporting in terms of ports
/// 0-15.
struct PortMap([PortLocation; NUM_PORTS as usize]);

impl core::ops::Index<LogicalPort> for PortMap {
    type Output = PortLocation;

    fn index(&self, i: LogicalPort) -> &Self::Output {
        &self.0[i.0 as usize]
    }
}

impl<'a> IntoIterator for &'a PortMap {
    type Item = &'a PortLocation;
    type IntoIter = core::slice::Iter<'a, PortLocation>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.as_slice().iter()
    }
}

impl PortMap {
    fn enumerate(&self) -> impl Iterator<Item = (LogicalPort, &PortLocation)> {
        self.0
            .iter()
            .enumerate()
            .map(|(i, v)| (LogicalPort(i as u8), v))
    }
}

const PORT_MAP: PortMap = PortMap([
    // Port 0
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(0),
    },
    // Port 1
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(1),
    },
    // Port 2
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(2),
    },
    // Port 3
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(3),
    },
    // Port 4
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(4),
    },
    // Port 5
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(5),
    },
    // Port 6
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(6),
    },
    // Port 7
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(7),
    },
    // Port 8
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(0),
    },
    // Port 9
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(1),
    },
    // Port 10
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(2),
    },
    // Port 11
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(3),
    },
    // Port 12
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(4),
    },
    // Port 13
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(5),
    },
    // Port 14
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(6),
    },
    // Port 15
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(7),
    },
    // Port 16
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(8),
    },
    // Port 17
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(9),
    },
    // Port 18
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(10),
    },
    // Port 19
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(11),
    },
    // Port 20
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(12),
    },
    // Port 21
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(13),
    },
    // Port 22
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(14),
    },
    // Port 23
    PortLocation {
        controller: FpgaController::Left,
        port: PhysicalPort(15),
    },
    // Port 24
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(8),
    },
    // Port 25
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(9),
    },
    // Port 26
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(10),
    },
    // Port 27
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(11),
    },
    // Port 28
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(12),
    },
    // Port 29
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(13),
    },
    // Port 30
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(14),
    },
    // Port 31
    PortLocation {
        controller: FpgaController::Right,
        port: PhysicalPort(15),
    },
]);

// These constants represent which logical locations are covered by each FPGA.
// This is convienient in the event that we cannot talk to one of the FPGAs as
// we can know which modules may be impacted.
const LEFT_LOGICAL_MASK: LogicalPortMask = LogicalPortMask(0x00ff00ff);
const RIGHT_LOGICAL_MASK: LogicalPortMask = LogicalPortMask(0xff00ff00);

/// Consolidates per-module success/failure/error information.
///
/// For operations which have no failure path, just success or error, make use
/// of the `ModuleResultNoFailure` type. Since multiple modules can be accessed
/// in parallel, we need to be able to handle a mix of the following cases on a
/// per-module basis:
/// - The module operation succeeded
/// - The module operation failed
/// - The module could not be interacted with due to an FPGA communication error
#[derive(Copy, Clone, Default, PartialEq)]
pub struct ModuleResult {
    success: LogicalPortMask,
    failure: LogicalPortMask,
    error: LogicalPortMask,
    failure_types: LogicalPortFailureTypes,
}

/// This is a slimmed down version of ModuleResult for use in the ringbuf
#[derive(Copy, Clone, PartialEq)]
pub struct ModuleResultSlim {
    success: LogicalPortMask,
    failure: LogicalPortMask,
    error: LogicalPortMask,
}

impl From<ModuleResultNoFailure> for ModuleResult {
    fn from(r: ModuleResultNoFailure) -> Self {
        ModuleResult::new(
            r.success(),
            LogicalPortMask(0),
            r.error(),
            LogicalPortFailureTypes::default(),
        )
        .unwrap_lite()
    }
}

impl ModuleResult {
    /// Enforces no overlap in the success, failure, and error masks.
    pub fn new(
        success: LogicalPortMask,
        failure: LogicalPortMask,
        error: LogicalPortMask,
        failure_types: LogicalPortFailureTypes,
    ) -> Result<Self, TransceiversError> {
        if !(success & failure).is_empty()
            || !(success & error).is_empty()
            || !(failure & error).is_empty()
        {
            return Err(TransceiversError::InvalidModuleResult);
        }
        Ok(Self {
            success,
            failure,
            error,
            failure_types,
        })
    }

    pub fn success(&self) -> LogicalPortMask {
        self.success
    }

    pub fn failure(&self) -> LogicalPortMask {
        self.failure
    }

    pub fn error(&self) -> LogicalPortMask {
        self.error
    }

    pub fn failure_types(&self) -> LogicalPortFailureTypes {
        self.failure_types
    }

    /// Combines two `ModuleResults`
    ///
    /// Intended to be used for a sequence of `ModuleResult` yielding function
    /// calls. Building such a sequence is generally done with the following
    /// form (where `modules` is a `LogicalPortMask` of requested modules):
    ///
    /// let result = some_result_fn(modules);
    /// let next_result = result.chain(another_result_fn(result.success()))
    ///
    /// So the initial result includes some set of success, failure, and error
    /// masks which then need to be reconciled with a new set of masks, generally
    /// a subset of the success mask of the initial result. Notably, there
    /// cannot be overlap between these masks, which this function enforces.
    ///
    /// # Panics
    ///
    /// This function panics if the `next.success` mask is not a subset of
    /// self.success. Additionally, it will panic if any of the success,
    /// failure, or error masks overlap with one another.
    pub fn chain<R: Into<ModuleResult>>(&self, next: R) -> Self {
        let next: ModuleResult = next.into();
        // success mask is just what the success of the next step is as long
        // as next.success is a subset of self.success, ensuring the semantics
        // of "chaining"
        assert!(next
            .success()
            .to_indices()
            .all(|idx| self.success().is_set(idx)));
        let success = next.success();
        // combine any new errors with the existing error mask
        let error = self.error() | next.error();
        // combine any new failures with the existing failure mask. Errors
        // supercede failures, so make sure to clear any failures where an error
        // has subsequently occurred.
        let failure = (self.failure() | next.failure()) & !self.error();

        // merge the failure types observed, prefering newer failure types
        // should both results have a failure at the same port.
        let mut combined_failures = LogicalPortFailureTypes::default();
        for p in failure.to_indices() {
            if next.failure().is_set(p) {
                combined_failures.0[p.0 as usize] =
                    next.failure_types().0[p.0 as usize];
            } else if self.failure().is_set(p) {
                combined_failures.0[p.0 as usize] =
                    self.failure_types().0[p.0 as usize];
            }
        }

        Self::new(success, failure, error, combined_failures).unwrap_lite()
    }

    /// Helper to provide a nice way to get a ModuleResultSlim from this result
    pub fn to_slim(&self) -> ModuleResultSlim {
        ModuleResultSlim {
            success: self.success(),
            failure: self.failure(),
            error: self.error(),
        }
    }
}

/// A type to consolidate per-module success/error information.
///
/// Since multiple modules can be accessed in parallel, we need to be able to
/// handle a mix of the following cases on a per-module basis:
/// - The module operation succeeded
/// - The module could not be interacted with due to an FPGA communication error
#[derive(Copy, Clone, Default, PartialEq)]
pub struct ModuleResultNoFailure {
    success: LogicalPortMask,
    error: LogicalPortMask,
}

impl ModuleResultNoFailure {
    /// Enforces no overlap in the success and error masks.
    pub fn new(
        success: LogicalPortMask,
        error: LogicalPortMask,
    ) -> Result<Self, TransceiversError> {
        if !(success & error).is_empty() {
            return Err(TransceiversError::InvalidModuleResult);
        }
        Ok(Self { success, error })
    }

    pub fn success(&self) -> LogicalPortMask {
        self.success
    }

    pub fn error(&self) -> LogicalPortMask {
        self.error
    }
}

/// A type to provide more ergonomic access to the FPGA generated type
pub type FpgaI2CFailure = Reg::QSFP::PORT0_STATUS::ErrorEncoded;

/// A type to consolidate per-module failure types.
///
/// Currently the only types of operations that can be considered failures are
/// those that involve the FPGA doing I2C. Thus, that is the only supported type
/// right now.
#[derive(Copy, Clone, PartialEq)]
pub struct LogicalPortFailureTypes(pub [FpgaI2CFailure; NUM_PORTS as usize]);

impl Default for LogicalPortFailureTypes {
    fn default() -> Self {
        Self([FpgaI2CFailure::NoError; NUM_PORTS as usize])
    }
}

impl core::ops::Index<LogicalPort> for LogicalPortFailureTypes {
    type Output = FpgaI2CFailure;

    fn index(&self, i: LogicalPort) -> &Self::Output {
        &self.0[i.0 as usize]
    }
}

#[derive(Copy, Clone)]
pub struct PortI2CStatus {
    pub stretching_seen: bool,
    pub rdata_fifo_empty: bool,
    pub wdata_fifo_empty: bool,
    pub done: bool,
    pub error: FpgaI2CFailure,
}

impl PortI2CStatus {
    pub fn new(status: u8) -> Self {
        // Use QSFP::PORT0 for constants, since they're all identical
        use Reg::QSFP::PORT0_STATUS;
        Self {
            stretching_seen: (status & PORT0_STATUS::STRETCHING_SEEN) != 0,
            rdata_fifo_empty: (status & PORT0_STATUS::RDATA_FIFO_EMPTY) != 0,
            wdata_fifo_empty: (status & PORT0_STATUS::WDATA_FIFO_EMPTY) != 0,
            done: (status & PORT0_STATUS::BUSY) == 0,
            error: FpgaI2CFailure::try_from(status).unwrap_lite(),
        }
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

    /// Executes a WriteOp (`op`) at `addr` for all ports per the `mask`.
    ///
    /// The meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn masked_port_op(
        &self,
        op: WriteOp,
        mask: LogicalPortMask,
        addr: Addr,
    ) -> ModuleResultNoFailure {
        let mut error = LogicalPortMask(0);
        // map the logical mask into a physical one
        let fpga_masks: FpgaPortMasks = mask.into();
        // talk to both FPGAs
        for fpga_index in fpga_masks.iter_fpgas() {
            let mask = fpga_masks.get(fpga_index);
            if !mask.is_empty() {
                let fpga = self.fpga(fpga_index);
                let wdata: U16<byteorder::LittleEndian> = U16::new(mask.get());
                // mark that an error occurred so we can modify the success mask
                if fpga.write(op, addr, wdata).is_err() {
                    error |= match fpga_index {
                        FpgaController::Left => LEFT_LOGICAL_MASK,
                        FpgaController::Right => RIGHT_LOGICAL_MASK,
                    }
                }
            }
        }
        // success is wherever we did not encounter an `FpgaError`
        let success = mask & !error;
        // only have an error where there was a requested module in mask
        error &= mask;

        ModuleResultNoFailure::new(success, error).unwrap_lite()
    }

    /// Set power enable bits per the specified `mask`.
    ///
    /// Controls whether or not a module's hot swap control will be turned on by
    /// the FPGA upon module insertion.
    /// The meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn enable_power(&self, mask: LogicalPortMask) -> ModuleResultNoFailure {
        self.masked_port_op(WriteOp::BitSet, mask, Addr::QSFP_SW_POWER_EN0)
    }

    /// Clear power enable bits per the specified `mask`.
    ///
    /// Controls whether or not a module's hot swap control will be turned on by
    /// the FPGA upon module insertion.
    /// The meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn disable_power(
        &self,
        mask: LogicalPortMask,
    ) -> ModuleResultNoFailure {
        self.masked_port_op(WriteOp::BitClear, mask, Addr::QSFP_SW_POWER_EN0)
    }

    /// Set ResetL bits per the specified `mask`.
    ///
    /// This directly controls the ResetL signal to the module.
    /// The meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn deassert_reset(
        &self,
        mask: LogicalPortMask,
    ) -> ModuleResultNoFailure {
        self.masked_port_op(WriteOp::BitSet, mask, Addr::QSFP_MOD_RESETL0)
    }

    /// Clear ResetL bits per the specified `mask`.
    ///
    /// This directly controls the ResetL signal to the module.
    /// The meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn assert_reset(&self, mask: LogicalPortMask) -> ModuleResultNoFailure {
        self.masked_port_op(WriteOp::BitClear, mask, Addr::QSFP_MOD_RESETL0)
    }

    /// Set LpMode bits per the specified `mask`.
    ///
    /// This directly controls the LpMode signal to the module.
    /// The meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn assert_lpmode(
        &self,
        mask: LogicalPortMask,
    ) -> ModuleResultNoFailure {
        self.masked_port_op(WriteOp::BitSet, mask, Addr::QSFP_MOD_LPMODE0)
    }

    /// Clear LpMode bits per the specified `mask`.
    ///
    /// This directly controls the LpMode signal to the module.
    /// The meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn deassert_lpmode(
        &self,
        mask: LogicalPortMask,
    ) -> ModuleResultNoFailure {
        self.masked_port_op(WriteOp::BitClear, mask, Addr::QSFP_MOD_LPMODE0)
    }

    /// Get the current status of all low speed signals for all ports.
    ///
    /// This is Enable, Reset, LpMode/TxDis, Power Good, Power Good Timeout,
    /// Present, and IRQ/RxLos.
    /// The meaning of the returned `ModuleResult`:
    /// success: we were able to read from the FPGA
    /// error: an `FpgaError` occurred
    pub fn get_module_status(&self) -> (ModuleStatus, ModuleResultNoFailure) {
        let ldata: Option<[U16<byteorder::LittleEndian>; 8]> = self
            .fpga(FpgaController::Left)
            .read(Addr::QSFP_POWER_EN0)
            .ok();
        let rdata: Option<[U16<byteorder::LittleEndian>; 8]> = self
            .fpga(FpgaController::Right)
            .read(Addr::QSFP_POWER_EN0)
            .ok();

        let mut status_masks: [u32; 8] = [0; 8];

        // loop through each logical port
        for port in (0..32).map(LogicalPort) {
            // Convert to a physical port using PORT_MAP
            let port_loc: PortLocation = port.into();

            // get the relevant data from the correct FPGA
            let local_data = match port_loc.controller {
                FpgaController::Left => ldata,
                FpgaController::Right => rdata,
            };
            let Some(local_data) = local_data else {
                continue;
            };

            // loop through the 8 different fields we need to map
            for (word, out) in local_data.iter().zip(status_masks.iter_mut()) {
                // if the bit is set, update our status mask at the correct
                // logical position
                let word: PhysicalPortMask = PhysicalPortMask((*word).into());
                if word.is_set(port_loc.port) {
                    *out |= 1 << port.0;
                }
            }
        }

        let success = match (ldata, rdata) {
            (None, None) => LogicalPortMask(0),
            (Some(_), None) => LEFT_LOGICAL_MASK,
            (None, Some(_)) => RIGHT_LOGICAL_MASK,
            (Some(_), Some(_)) => LEFT_LOGICAL_MASK | RIGHT_LOGICAL_MASK,
        };
        let error = !success;

        (
            ModuleStatus::read_from(status_masks.as_bytes()).unwrap_lite(),
            ModuleResultNoFailure::new(success, error).unwrap_lite(),
        )
    }

    /// Clear a fault for each port per the specified `mask`.
    ///
    /// The meaning of the
    /// returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn clear_power_fault(
        &self,
        mask: LogicalPortMask,
    ) -> ModuleResultNoFailure {
        let mut error = LogicalPortMask(0);
        // map the logical mask into the physical one
        let fpga_masks: FpgaPortMasks = mask.into();
        // talk to both FPGAs
        for fpga_index in fpga_masks.iter_fpgas() {
            let mask = fpga_masks.get(fpga_index);
            if !mask.is_empty() {
                let fpga = self.fpga(fpga_index);
                for port in 0..16 {
                    if mask.is_set(PhysicalPort(port))
                        && fpga
                            .write(
                                WriteOp::BitSet,
                                Addr::QSFP_PORT0_CONTROL as u16
                                    + u16::from(port),
                                Reg::QSFP::PORT0_CONTROL::CLEAR_FAULT,
                            )
                            .is_err()
                    {
                        error |= match fpga_index {
                            FpgaController::Left => LEFT_LOGICAL_MASK,
                            FpgaController::Right => RIGHT_LOGICAL_MASK,
                        }
                    }
                }
            }
        }
        // success is wherever we did not encounter an `FpgaError`
        let success = mask & !error;
        // only have an error where there was a requested module in mask
        error &= mask;

        ModuleResultNoFailure::new(success, error).unwrap_lite()
    }

    /// Initiate an I2C random read on all ports per the specified `mask`.
    ///
    /// The maximum value of `num_bytes` is 128.The meaning of the returned
    /// `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn setup_i2c_read(
        &self,
        reg: u8,
        num_bytes: u8,
        mask: LogicalPortMask,
    ) -> ModuleResultNoFailure {
        self.setup_i2c_op(true, reg, num_bytes, mask)
    }

    /// Initiate an I2C write on all ports per the specified `mask`.
    ///
    /// The maximum value of `num_bytes` is 128. The meaning of the
    /// returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn setup_i2c_write(
        &self,
        reg: u8,
        num_bytes: u8,
        mask: LogicalPortMask,
    ) -> ModuleResultNoFailure {
        self.setup_i2c_op(false, reg, num_bytes, mask)
    }

    /// Initiate an I2C operation on all ports per the specified `mask`.
    ///
    /// When `is_read` is true, the operation will be a random-read, not a pure
    /// I2C read. The maximum value of `num_bytes` is 128.
    /// The meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn setup_i2c_op(
        &self,
        is_read: bool,
        reg: u8,
        num_bytes: u8,
        mask: LogicalPortMask,
    ) -> ModuleResultNoFailure {
        let fpga_masks: FpgaPortMasks = mask.into();
        let mut success = LogicalPortMask(0);

        let i2c_op = if is_read {
            // Defaulting to RandomRead, rather than Read, because RandomRead
            // sets the reg addr in the target device, then issues a restart to
            // do the read at that reg addr. On the other hand, Read just starts
            // a read wherever the reg addr is after the last transaction.
            TransceiverI2COperation::RandomRead
        } else {
            TransceiverI2COperation::Write
        };

        if !fpga_masks.left.is_empty() {
            let request = TransceiversI2CRequest {
                reg,
                num_bytes,
                mask: U16::new(fpga_masks.left.0),
                op: i2c_op as u8,
            };

            if self
                .fpga(FpgaController::Left)
                .write(WriteOp::Write, Addr::QSFP_I2C_REG_ADDR, request)
                .is_ok()
            {
                success |= LEFT_LOGICAL_MASK;
            }
        }

        if !fpga_masks.right.is_empty() {
            let request = TransceiversI2CRequest {
                reg,
                num_bytes,
                mask: U16::new(fpga_masks.right.0),
                op: i2c_op as u8,
            };
            if self
                .fpga(FpgaController::Right)
                .write(WriteOp::Write, Addr::QSFP_I2C_REG_ADDR, request)
                .is_ok()
            {
                success |= RIGHT_LOGICAL_MASK;
            }
        }

        success &= mask;
        let error = mask & !success;

        ModuleResultNoFailure::new(success, error).unwrap_lite()
    }

    /// Read the value of the QSFP_PORTx_STATUS
    ///
    /// This contains information on if
    /// the I2C core is busy or if there were any errors with the transaction.
    pub fn get_i2c_status<P: Into<PortLocation>>(
        &self,
        port: P,
    ) -> Result<u8, FpgaError> {
        let port_loc = port.into();
        self.fpga(port_loc.controller)
            .read(Addr::QSFP_PORT0_STATUS as u16 + port_loc.port.0 as u16)
    }

    /// Returns a PortI2CStatus and will fill `buf.len()` bytes of data with the
    /// I2C read buffer for a `port`. The buffer stores data from the last I2C
    /// read transaction done and thus only the number of bytes read will be
    /// valid in the buffer.
    ///
    /// Poll the status register for a port until an I2C transaction is done.
    /// Upon completion, return the status struct back and the read data. The
    /// read data buffer stores I2C read data from the last transaction so the
    /// FIFO will only contain that many bytes. The FPGA automatically clears
    /// the read data FIFO before starting a new read, so there's no need for
    /// the SP to do so.
    pub fn get_i2c_status_and_read_buffer<P: Into<PortLocation>>(
        &self,
        port: P,
        buf: &mut [u8],
    ) -> Result<PortI2CStatus, FpgaError> {
        let port_loc = port.into();
        loop {
            let status_byte = self.get_i2c_status(port_loc)?;
            let status = PortI2CStatus::new(status_byte);

            if status.done {
                self.fpga(port_loc.controller).read_bytes(
                    ReadOp::ReadNoAddrIncr,
                    Addr::QSFP_PORT0_I2C_DATA as u16 + port_loc.port.0 as u16,
                    buf,
                )?;
                return Ok(status);
            }

            userlib::hl::sleep_for(1);
        }
    }

    /// Write `buf.len()` bytes of data into the I2C write buffer of the `mask`
    ///
    /// Each port has a FIFO of write data that the I2C core will write to a
    /// target on the next write operation. This function will clear the FIFO
    /// prior to writing in the new data. This In the event of an error during
    /// FPGA communication, a `LogicalPortMask` representing the affected ports
    /// is included.
    ///
    /// The meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    pub fn set_i2c_write_buffer(
        &self,
        buf: &[u8],
        mask: LogicalPortMask,
    ) -> ModuleResultNoFailure {
        let mut error = LogicalPortMask(0);

        // for every port in the mask
        for port in mask.to_indices() {
            // we need to know the FPGA and which physical port
            let loc = port.get_physical_location();
            let fpga = self.fpga(loc.controller);
            let port_addr = loc.port.0 as u16;
            // clear the FIFO
            if fpga
                .write(
                    WriteOp::BitSet,
                    Addr::QSFP_PORT0_CONTROL as u16 + port_addr,
                    Reg::QSFP::PORT0_CONTROL::WDATA_FIFO_CLEAR,
                )
                .is_ok()
            {
                // write in the buffer
                if fpga
                    .write_bytes(
                        WriteOp::WriteNoAddrIncr,
                        Addr::QSFP_PORT0_I2C_DATA as u16 + port_addr,
                        buf,
                    )
                    .is_err()
                {
                    error |= port.as_mask();
                }
            } else {
                error |= port.as_mask();
            }
        }
        // success is wherever we did not encounter an `FpgaError`
        let success = mask & !error;

        ModuleResultNoFailure::new(success, error).unwrap_lite()
    }

    /// Apply reset to the LED controller
    ///
    /// Per section 7.6 of the datasheet the minimum required pulse width here
    /// is 2.5 microseconds. Given the SPI interface runs at 3MHz, the
    /// transaction to clear the reset would take ~10 microseconds on its own,
    /// so there is no additional delay here.
    pub fn assert_led_controllers_reset(&mut self) -> Result<(), FpgaError> {
        for fpga in &self.fpgas {
            fpga.write(WriteOp::BitSet, Addr::LED_CTRL, Reg::LED_CTRL::RESET)?;
        }
        Ok(())
    }

    /// Remove reset from the LED controller
    ///
    /// Per section 7.6 of the datasheet the device has a maximum wait time of
    /// 1.5 milliseconds after the release of reset to normal operation, so
    /// there is a 2 millisecond wait here.
    pub fn deassert_led_controllers_reset(&mut self) -> Result<(), FpgaError> {
        for fpga in &self.fpgas {
            fpga.write(
                WriteOp::BitClear,
                Addr::LED_CTRL,
                Reg::LED_CTRL::RESET,
            )?;
        }
        userlib::hl::sleep_for(2);
        Ok(())
    }

    /// Releases the LED controller from reset and enables the output
    pub fn enable_led_controllers(&mut self) -> Result<(), FpgaError> {
        self.deassert_led_controllers_reset()?;
        for fpga in &self.fpgas {
            fpga.write(WriteOp::BitSet, Addr::LED_CTRL, Reg::LED_CTRL::OE)?;
        }
        Ok(())
    }

    /// Waits for all of the I2C busy bits to go low
    ///
    /// Returns a set of masks indicating which channels (among the ones active
    /// in the input mask) have recorded FPGA errors. The meaning of the
    /// returned `ModuleResult`:
    /// success: the desired I2C transaction completed successfully
    /// failure: there was an I2C error
    /// error: an `FpgaError` occurred
    pub fn wait_and_check_i2c(
        &mut self,
        mask: LogicalPortMask,
    ) -> ModuleResult {
        let mut physical_failure = FpgaPortMasks::default();
        let mut physical_error = FpgaPortMasks::default();
        let phys_mask: FpgaPortMasks = mask.into();
        let mut failure_types = LogicalPortFailureTypes::default();

        #[derive(IntoBytes, Default, FromBytes)]
        #[repr(C)]
        struct BusyAndPortStatus {
            busy: u16,
            status: [u8; 16],
        }
        for fpga_index in phys_mask.iter_fpgas() {
            let fpga = self.fpga(fpga_index);
            // This loop should break immediately, because I2C is fast
            let status_all = loop {
                // Two bytes of BUSY, followed by 816bytes of port status
                let status_all = match fpga.read(Addr::QSFP_I2C_BUSY0) {
                    Ok(data) => data,
                    // If there is an FPGA communication error, mark that as an
                    // error on all of that FPGA's ports
                    Err(_) => {
                        *physical_error.get_mut(fpga_index) =
                            PhysicalPortMask(0xffff);
                        BusyAndPortStatus::default()
                    }
                };
                if status_all.busy == 0 {
                    break status_all;
                }
                userlib::hl::sleep_for(1);
            };

            // Check errors on a per-port basis
            let phys_mask = match fpga_index {
                FpgaController::Left => phys_mask.left,
                FpgaController::Right => phys_mask.right,
            };
            for port in (0..16).map(PhysicalPort) {
                // skip ports we didn't interact with
                if !phys_mask.is_set(port) {
                    continue;
                }

                // cast status byte into a type
                let failure = FpgaI2CFailure::try_from(
                    status_all.status[port.0 as usize],
                )
                .unwrap_lite();

                // if a failure occurred, mark it and record the failure type
                if failure != FpgaI2CFailure::NoError {
                    physical_failure.get_mut(fpga_index).set(port);
                    let logical_port =
                        port.to_logical_port(fpga_index).unwrap_lite();
                    failure_types.0[logical_port.0 as usize] = failure;
                }
            }
        }
        let error = mask & LogicalPortMask::from(physical_error);
        let failure = mask & LogicalPortMask::from(physical_failure);
        let success = mask & !(error | failure);
        ModuleResult::new(success, failure, error, failure_types).unwrap_lite()
    }
}

// The I2C control register looks like:
// [2..1] - Operation (0 - Read, 1 - Write, 2 - RandomRead)
// [0] - Start
#[derive(Copy, Clone, Debug, IntoBytes)]
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

#[derive(IntoBytes, FromBytes, Unaligned)]
#[repr(C)]
pub struct TransceiversI2CRequest {
    reg: u8,
    num_bytes: u8,
    mask: U16<byteorder::LittleEndian>,
    op: u8,
}
