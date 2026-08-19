// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Engine for diagnosing issues with the sequencer failing to reach A0
//!
//! Based on the A0 sequencer fault tree in
//! [Quartz](https://github.com/oxidecomputer/quartz/blob/ndh/a0-fault-tree/hdl/projects/cosmo_seq/sequencer/docs/a0_sequencing_fault_tree.md)

use crate::fmc_sequencer::*;
use ringbuf::{counted_ringbuf, ringbuf, ringbuf_entry};

#[derive(Copy, Clone, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    UnknownHwSmState(u8),
    UnknownA0SmState(u8),
    SequencerIsDone,
    IntermediateHwSmState(seq_raw_status::HwSm),
    BadStateCombination {
        a0_sm: seq_api_status::A0Sm,
        hw_sm: seq_raw_status::HwSm,
    },
    Diagnosis {
        now: u64,
        reason: DiagnoseReason,
        #[count(children)]
        details: Diagnosis,
    },
}

#[derive(Copy, Clone, PartialEq)]
enum RawRegisterTrace {
    None,
    Registers {
        now: u64,
        reason: DiagnoseReason,
        values: RegisterDump,
    },
}
counted_ringbuf!(Trace, 8, Trace::None);
ringbuf!(RAW, RawRegisterTrace, 8, RawRegisterTrace::None);

/// Loggable enum explaining a power sequencing failure
#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum Diagnosis {
    StuckInIdle {
        #[count(children)]
        why: WhyStuckInIdle,
        a0_en: bool,
    },
    WaitingForGroupA {
        #[count(children)]
        why: WhyWaitingForGroupA,
    },
    WaitingForSlpCheckpoint {
        #[count(children)]
        why: WhyWaitingForSlpCheckpoint,
    },
    WaitingForGroupB {
        #[count(children)]
        why: WhyWaitingForGroupB,
    },
    WaitingForGroupC {
        #[count(children)]
        why: WhyWaitingForGroupC,
    },
    WaitingForPowerOk {
        #[count(children)]
        why: WhyWaitingForPowerOk,
        if_you_are_testing_without_sp5_this_must_be_true: bool,
    },
    WaitingForResetLRelease {
        #[count(children)]
        why: WhyWaitForResetLRelease,
        if_you_are_testing_without_sp5_this_must_be_true: bool,
    },
    Mapo {
        #[count(children)]
        why: WhyMapo,
    },
    SoftwareDisable {
        a0_sm: seq_api_status::A0Sm,
        a0_en: bool,
        a0mapo: bool,
    },
}

#[derive(Copy, Clone, PartialEq)]
pub(crate) struct RegisterDump {
    seq_api_status: SeqApiStatusView,
    seq_raw_status: SeqRawStatusView,
    early_power_rdbks: EarlyPowerRdbksView,
    ifr: IfrView,
    debug_enables: DebugEnablesView,
    power_ctrl: PowerCtrlView,
    rail_enables: RailEnablesView,
    rail_pgs: RailPgsView,
    rail_pgs_max_hold: RailPgsMaxHoldView,
    sp5_readbacks: Sp5ReadbacksView,
    status: StatusView,
}

/// Raw registers to be sent as an ereport
#[derive(Copy, Clone, PartialEq, microcbor::Encode)]
#[ereport(class = "hw.seq.regs", version = 0)]
pub(crate) struct RawRegisterDump {
    seq_api_status: u32,
    seq_raw_status: u32,
    early_power_rdbks: u32,
    ifr: u32,
    debug_enables: u32,
    power_ctrl: u32,
    rail_enables: u32,
    rail_pgs: u32,
    rail_pgs_max_hold: u32,
    sp5_readbacks: u32,
    status: u32,

    reason: DiagnoseReason,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum WhyStuckInIdle {
    FanHscNotPg(FanHsc),
    FanPowerNotOk,
    A0EnNotSet,
    Unknown,
}

#[derive(Copy, Clone, PartialEq)]
pub(crate) struct RailStatus {
    enabled: bool,
    power_good: bool,
    power_good_max_hold: bool,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
pub(crate) enum RailIssue {
    NotEnabled,
    NotPowerGood,
    PowerGoodIntermittent,
}

#[derive(Copy, Clone, PartialEq, microcbor::Encode)]
#[ereport(class = "hw.seq.timeout.group_a", version = 0)]
pub(crate) struct GroupATimeoutEreport {
    err: WhyWaitingForGroupA,
    regs_ena: Option<u64>,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
pub(crate) enum WhyWaitingForGroupA {
    RailIssue(#[count(children)] RailIssue, GroupARail),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, microcbor::Encode)]
#[ereport(class = "hw.seq.timeout.slp_checkpoint", version = 0)]
pub(crate) struct SlpCheckpointTimeoutEreport {
    err: WhyWaitingForSlpCheckpoint,
    regs_ena: Option<u64>,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
pub(crate) enum WhyWaitingForSlpCheckpoint {
    Sp5StuckInS5Sleep,
    Sp5StuckInS3Sleep,
    RsmRstLNotReleased,
    PowerButtonStillAsserted,
    RailIssue(#[count(children)] RailIssue, Ddr5HscRail),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, microcbor::Encode)]
#[ereport(class = "hw.seq.timeout.group_b", version = 0)]
pub(crate) struct GroupBTimeoutEreport {
    err: WhyWaitingForGroupB,
    regs_ena: Option<u64>,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
pub(crate) enum WhyWaitingForGroupB {
    RailIssue(#[count(children)] RailIssue, GroupBRail),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, microcbor::Encode)]
#[ereport(class = "hw.seq.timeout.group_c", version = 0)]
pub(crate) struct GroupCTimeoutEreport {
    err: WhyWaitingForGroupC,
    regs_ena: Option<u64>,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
pub(crate) enum WhyWaitingForGroupC {
    RailIssue(#[count(children)] RailIssue, GroupCRail),
    VrControllerAlert(u8),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, microcbor::Encode)]
#[ereport(class = "hw.seq.timeout.power_ok", version = 0)]
pub(crate) struct PowerOkTimeoutEreport {
    err: WhyWaitingForPowerOk,
    regs_ena: Option<u64>,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
pub(crate) enum WhyWaitingForPowerOk {
    Sp5NotAssertingPowerOk,
    FpgaNotDrivingPowerGood,
    RailIssue(#[count(children)] RailIssue, Rail),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, microcbor::Encode)]
#[ereport(class = "hw.seq.timeout.reset_l_release", version = 0)]
pub(crate) struct ResetLReleaseTimeoutEreport {
    err: WhyWaitForResetLRelease,
    regs_ena: Option<u64>,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
pub(crate) enum WhyWaitForResetLRelease {
    Sp5HoldingResetLow,
    Sp5DroppedPwrOk,
    Unknown,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum FanHsc {
    East,
    Central,
    West,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum WhyMapo {
    GroupAMapo(RailIssue, #[count(children)] GroupARail),
    GroupBMapo(RailIssue, #[count(children)] GroupBRail),
    GroupCMapo(RailIssue, #[count(children)] GroupCRail),
    Ddr5HscNotPg(Ddr5HscRail),
    FanHscNotPg(FanHsc),
    VrControllerAlert(u8),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
#[allow(non_camel_case_types)]
pub(crate) enum GroupARail {
    V1P5_RTC,
    V3P3_SP5_A1,
    V1P8_SP5_A1,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
#[allow(non_camel_case_types)]
pub(crate) enum GroupBRail {
    V1P1_SP5,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
#[allow(non_camel_case_types)]
pub(crate) enum Ddr5HscRail {
    DDR5_ABCDEF,
    DDR5_GHIJKL,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
#[allow(non_camel_case_types)]
pub(crate) enum GroupCRail {
    VDDIO_SP5_A0,
    VDDCR_CPU0,
    VDDCR_CPU1,
    VDDCR_SOC,
}

#[derive(Copy, Clone, PartialEq, counters::Count, microcbor::Encode)]
pub(crate) enum Rail {
    GroupA(#[count(children)] GroupARail),
    GroupB(#[count(children)] GroupBRail),
    GroupC(#[count(children)] GroupCRail),
}

fn get_rail_issue<T: Copy>(
    rails: &[(RailStatus, T)],
) -> Option<(RailIssue, T)> {
    if let Some((_r, t)) = rails.iter().find(|(r, _)| !r.enabled) {
        Some((RailIssue::NotEnabled, *t))
    } else if let Some((_r, t)) = rails
        .iter()
        .find(|(r, _)| !r.power_good && r.power_good_max_hold)
    {
        Some((RailIssue::PowerGoodIntermittent, *t))
    } else if let Some((_r, t)) = rails.iter().find(|(r, _)| !r.power_good) {
        Some((RailIssue::NotPowerGood, *t))
    } else {
        None
    }
}

/// Reason why the top-level sequencer code called for a diagnosis
#[derive(Copy, Clone, Debug, PartialEq, microcbor::Encode)]
pub(crate) enum DiagnoseReason {
    FailedToSequence,
    MapoDetected,
    UnexpectedPowerOff,
}

/// Diagnoses a problem with the sequencer failing to get to A0
///
/// The result is logged in a ringbuf
pub(crate) fn a0_fault(
    seq: &Sequencer,
    reason: DiagnoseReason,
    now: u64,
    ereporter: &mut crate::Ereporter,
) {
    // Get raw (u32) register values
    let raw = RawRegisterDump {
        seq_raw_status: seq.seq_raw_status.get_raw(),
        seq_api_status: seq.seq_api_status.get_raw(),
        power_ctrl: seq.power_ctrl.get_raw(),
        early_power_rdbks: seq.early_power_rdbks.get_raw(),
        status: seq.status.get_raw(),
        rail_enables: seq.rail_enables.get_raw(),
        rail_pgs: seq.rail_pgs.get_raw(),
        rail_pgs_max_hold: seq.rail_pgs_max_hold.get_raw(),
        sp5_readbacks: seq.sp5_readbacks.get_raw(),
        debug_enables: seq.debug_enables.get_raw(),
        ifr: seq.ifr.get_raw(),

        reason,
    };

    // Send the raw registers as an ereport; record the ENA to send in
    // subsequent ereports (sometimes)
    let regs_ena = ereporter.deliver_ereport(&raw).ok().map(|r| r.0.into());

    // Convert to view values
    let seq_raw_status = SeqRawStatusView::from(raw.seq_raw_status);
    let seq_api_status = SeqApiStatusView::from(raw.seq_api_status);
    let power_ctrl = PowerCtrlView::from(raw.power_ctrl);
    let early_power_rdbks = EarlyPowerRdbksView::from(raw.early_power_rdbks);
    let status = StatusView::from(raw.status);
    let rail_enables = RailEnablesView::from(raw.rail_enables);
    let rail_pgs = RailPgsView::from(raw.rail_pgs);
    let rail_pgs_max_hold = RailPgsMaxHoldView::from(raw.rail_pgs_max_hold);
    let sp5_readbacks = Sp5ReadbacksView::from(raw.sp5_readbacks);
    let debug_enables = DebugEnablesView::from(raw.debug_enables);
    let ifr = IfrView::from(raw.ifr);

    ringbuf_entry!(
        RAW,
        RawRegisterTrace::Registers {
            now,
            reason,
            values: RegisterDump {
                seq_raw_status,
                seq_api_status,
                power_ctrl,
                early_power_rdbks,
                status,
                rail_enables,
                rail_pgs,
                rail_pgs_max_hold,
                sp5_readbacks,
                debug_enables,
                ifr,
            }
        }
    );

    // Get the a0_sm and hw_sm fields as nicely typed enums.  If they are an
    // invalid value, then we can't proceed any further with diagnosis.
    let a0_sm = match seq_api_status.a0_sm {
        Ok(a) => a,
        Err(v) => {
            ringbuf_entry!(Trace::UnknownA0SmState(v));
            return;
        }
    };
    let hw_sm = match seq_raw_status.hw_sm {
        Ok(a) => a,
        Err(v) => {
            ringbuf_entry!(Trace::UnknownHwSmState(v));
            return;
        }
    };

    // Helper tools to extract individual rail states
    struct RailState {
        enables: RailEnablesView,
        power_good: RailPgsView,
        power_good_max_hold: RailPgsMaxHoldView,
    }

    let rail_state = RailState {
        enables: rail_enables,
        power_good: rail_pgs,
        power_good_max_hold: rail_pgs_max_hold,
    };
    macro_rules! rail_status {
        ($state:ident, ($($name:ident),+)) => {
            (
                $(
                RailStatus {
                    enabled: $state.enables.$name,
                    power_good: $state.power_good.$name,
                    power_good_max_hold: $state.power_good_max_hold.$name,
                },
                )+
            )
        }
    }

    use seq_api_status::A0Sm;
    use seq_raw_status::HwSm;
    let details = match hw_sm {
        HwSm::Idle => {
            // Ironically, this is the most complicated one!
            match a0_sm {
                A0Sm::Disabling => Diagnosis::SoftwareDisable {
                    a0_sm,
                    a0_en: power_ctrl.a0_en,
                    a0mapo: ifr.a0mapo,
                },
                A0Sm::Faulted => {
                    // Faulted means we diagnose a MAPO condition, which
                    // requires checking every single rail.
                    use GroupARail::*;
                    use GroupBRail::*;
                    use GroupCRail::*;
                    let (v1p5_rtc, v3p3_sp5, v1p8_sp5) = rail_status!(
                        rail_state,
                        (v1p5_rtc, v3p3_sp5, v1p8_sp5)
                    );
                    let ra = get_rail_issue(&[
                        (v1p5_rtc, V1P5_RTC),
                        (v3p3_sp5, V3P3_SP5_A1),
                        (v1p8_sp5, V1P8_SP5_A1),
                    ]);
                    let (v1p1_sp5,) = rail_status!(rail_state, (v1p1_sp5));
                    let rb = get_rail_issue(&[(v1p1_sp5, V1P1_SP5)]);
                    let (sp5, cpu0, cpu1, soc) = rail_status!(
                        rail_state,
                        (vddio_sp5, vddcr_cpu0, vddcr_cpu1, vddcr_soc)
                    );
                    let rc = get_rail_issue(&[
                        (sp5, VDDIO_SP5_A0),
                        (cpu0, VDDCR_CPU0),
                        (cpu1, VDDCR_CPU1),
                        (soc, VDDCR_SOC),
                    ]);
                    let why = if let Some((i, r)) = ra {
                        WhyMapo::GroupAMapo(i, r)
                    } else if let Some((i, r)) = rb {
                        WhyMapo::GroupBMapo(i, r)
                    } else if let Some((i, r)) = rc {
                        WhyMapo::GroupCMapo(i, r)
                    } else if !early_power_rdbks.fan_hsc_east_pg {
                        WhyMapo::FanHscNotPg(FanHsc::East)
                    } else if !early_power_rdbks.fan_hsc_central_pg {
                        WhyMapo::FanHscNotPg(FanHsc::Central)
                    } else if !early_power_rdbks.fan_hsc_west_pg {
                        WhyMapo::FanHscNotPg(FanHsc::West)
                    } else if !rail_pgs.abcdef_hsc {
                        WhyMapo::Ddr5HscNotPg(Ddr5HscRail::DDR5_ABCDEF)
                    } else if !rail_pgs.ghijkl_hsc {
                        WhyMapo::Ddr5HscNotPg(Ddr5HscRail::DDR5_GHIJKL)
                    } else if ifr.pwr_cont1_to_fpga1_alert {
                        WhyMapo::VrControllerAlert(1)
                    } else if ifr.pwr_cont2_to_fpga1_alert {
                        WhyMapo::VrControllerAlert(2)
                    } else if ifr.pwr_cont3_to_fpga1_alert {
                        WhyMapo::VrControllerAlert(3)
                    } else {
                        WhyMapo::Unknown
                    };
                    Diagnosis::Mapo { why }
                }
                A0Sm::Idle => {
                    let why = if !early_power_rdbks.fan_hsc_east_pg {
                        WhyStuckInIdle::FanHscNotPg(FanHsc::East)
                    } else if !early_power_rdbks.fan_hsc_central_pg {
                        WhyStuckInIdle::FanHscNotPg(FanHsc::Central)
                    } else if !early_power_rdbks.fan_hsc_west_pg {
                        WhyStuckInIdle::FanHscNotPg(FanHsc::West)
                    } else if !status.fanpwrok {
                        WhyStuckInIdle::FanPowerNotOk
                    } else if !power_ctrl.a0_en {
                        WhyStuckInIdle::A0EnNotSet
                    } else {
                        WhyStuckInIdle::Unknown
                    };
                    Diagnosis::StuckInIdle {
                        why,
                        a0_en: power_ctrl.a0_en,
                    }
                }
                _ => {
                    ringbuf_entry!(Trace::BadStateCombination { hw_sm, a0_sm });
                    return;
                }
            }
        }
        HwSm::GroupAPgAndWait => {
            use GroupARail::*;
            let (v1p5_rtc, v3p3_sp5, v1p8_sp5) =
                rail_status!(rail_state, (v1p5_rtc, v3p3_sp5, v1p8_sp5));
            let ri = get_rail_issue(&[
                (v1p5_rtc, V1P5_RTC),
                (v3p3_sp5, V3P3_SP5_A1),
                (v1p8_sp5, V1P8_SP5_A1),
            ]);
            let why = ri
                .map(|(i, r)| WhyWaitingForGroupA::RailIssue(i, r))
                .unwrap_or(WhyWaitingForGroupA::Unknown);
            if reason == DiagnoseReason::FailedToSequence {
                let _ = ereporter.deliver_ereport(&GroupATimeoutEreport {
                    err: why,
                    regs_ena,
                });
            }
            Diagnosis::WaitingForGroupA { why }
        }
        HwSm::SlpCheckpoint => {
            let (ddr5_abcdef, ddr5_ghijkl) =
                rail_status!(rail_state, (abcdef_hsc, ghijkl_hsc));
            let why = if !sp5_readbacks.slp_s5_l {
                WhyWaitingForSlpCheckpoint::Sp5StuckInS5Sleep
            } else if !sp5_readbacks.slp_s3_l {
                WhyWaitingForSlpCheckpoint::Sp5StuckInS3Sleep
            } else if !sp5_readbacks.rsmrst_l {
                WhyWaitingForSlpCheckpoint::RsmRstLNotReleased
            } else if !sp5_readbacks.pwr_btn_l {
                WhyWaitingForSlpCheckpoint::PowerButtonStillAsserted
            } else {
                use Ddr5HscRail::*;
                get_rail_issue(&[
                    (ddr5_abcdef, DDR5_ABCDEF),
                    (ddr5_ghijkl, DDR5_GHIJKL),
                ])
                .map(|(i, r)| WhyWaitingForSlpCheckpoint::RailIssue(i, r))
                .unwrap_or(WhyWaitingForSlpCheckpoint::Unknown)
            };
            if reason == DiagnoseReason::FailedToSequence {
                let _ =
                    ereporter.deliver_ereport(&SlpCheckpointTimeoutEreport {
                        err: why,
                        regs_ena,
                    });
            }
            Diagnosis::WaitingForSlpCheckpoint { why }
        }
        HwSm::GroupBPgAndWait => {
            let (v1p1_sp5,) = rail_status!(rail_state, (v1p1_sp5));
            let why = get_rail_issue(&[(v1p1_sp5, GroupBRail::V1P1_SP5)])
                .map(|(i, r)| WhyWaitingForGroupB::RailIssue(i, r))
                .unwrap_or(WhyWaitingForGroupB::Unknown);
            if reason == DiagnoseReason::FailedToSequence {
                let _ = ereporter.deliver_ereport(&GroupBTimeoutEreport {
                    err: why,
                    regs_ena,
                });
            }
            Diagnosis::WaitingForGroupB { why }
        }
        HwSm::GroupCPgAndWait => {
            let (vddio_sp5, vddcr_cpu0, vddcr_cpu1, vddcr_soc) = rail_status!(
                rail_state,
                (vddio_sp5, vddcr_cpu0, vddcr_cpu1, vddcr_soc)
            );
            let why = if ifr.pwr_cont1_to_fpga1_alert {
                WhyWaitingForGroupC::VrControllerAlert(1)
            } else if ifr.pwr_cont2_to_fpga1_alert {
                WhyWaitingForGroupC::VrControllerAlert(2)
            } else {
                use GroupCRail::*;
                get_rail_issue(&[
                    (vddio_sp5, VDDIO_SP5_A0),
                    (vddcr_cpu0, VDDCR_CPU0),
                    (vddcr_cpu1, VDDCR_CPU1),
                    (vddcr_soc, VDDCR_SOC),
                ])
                .map(|(i, r)| WhyWaitingForGroupC::RailIssue(i, r))
                .unwrap_or(WhyWaitingForGroupC::Unknown)
            };
            if reason == DiagnoseReason::FailedToSequence {
                let _ = ereporter.deliver_ereport(&GroupCTimeoutEreport {
                    err: why,
                    regs_ena,
                });
            }
            Diagnosis::WaitingForGroupC { why }
        }
        HwSm::WaitPwrok => {
            use GroupARail::*;
            use GroupBRail::*;
            use GroupCRail::*;
            let (
                v1p5_rtc,
                v3p3_sp5,
                v1p8_sp5,
                v1p1_sp5,
                vddio_sp5,
                vddcr_cpu0,
                vddcr_cpu1,
                vddcr_soc,
            ) = rail_status!(
                rail_state,
                (
                    v1p5_rtc, v3p3_sp5, v1p8_sp5, v1p1_sp5, vddio_sp5,
                    vddcr_cpu0, vddcr_cpu1, vddcr_soc
                )
            );
            let why = if !sp5_readbacks.pwr_ok {
                WhyWaitingForPowerOk::Sp5NotAssertingPowerOk
            } else if !sp5_readbacks.pwr_good {
                WhyWaitingForPowerOk::FpgaNotDrivingPowerGood
            } else if let Some((issue, rail)) = get_rail_issue(&[
                (v1p5_rtc, Rail::GroupA(V1P5_RTC)),
                (v3p3_sp5, Rail::GroupA(V3P3_SP5_A1)),
                (v1p8_sp5, Rail::GroupA(V1P8_SP5_A1)),
                (v1p1_sp5, Rail::GroupB(V1P1_SP5)),
                (vddio_sp5, Rail::GroupC(VDDIO_SP5_A0)),
                (vddcr_cpu0, Rail::GroupC(VDDCR_CPU0)),
                (vddcr_cpu1, Rail::GroupC(VDDCR_CPU1)),
                (vddcr_soc, Rail::GroupC(VDDCR_SOC)),
            ]) {
                WhyWaitingForPowerOk::RailIssue(issue, rail)
            } else {
                WhyWaitingForPowerOk::Unknown
            };
            if reason == DiagnoseReason::FailedToSequence {
                let _ = ereporter.deliver_ereport(&PowerOkTimeoutEreport {
                    err: why,
                    regs_ena,
                });
            }
            Diagnosis::WaitingForPowerOk {
                why,
                if_you_are_testing_without_sp5_this_must_be_true: debug_enables
                    .ignore_sp5,
            }
        }
        HwSm::WaitResetLRelease => {
            let why = if !sp5_readbacks.reset_l {
                WhyWaitForResetLRelease::Sp5HoldingResetLow
            } else if !sp5_readbacks.pwr_ok {
                WhyWaitForResetLRelease::Sp5DroppedPwrOk
            } else {
                WhyWaitForResetLRelease::Unknown
            };
            if reason == DiagnoseReason::FailedToSequence {
                let _ =
                    ereporter.deliver_ereport(&ResetLReleaseTimeoutEreport {
                        err: why,
                        regs_ena,
                    });
            }
            Diagnosis::WaitingForResetLRelease {
                why,
                if_you_are_testing_without_sp5_this_must_be_true: debug_enables
                    .ignore_sp5,
            }
        }
        HwSm::Done => {
            ringbuf_entry!(Trace::SequencerIsDone);
            return;
        }
        HwSm::SafeDisable => Diagnosis::SoftwareDisable {
            a0_sm,
            a0_en: power_ctrl.a0_en,
            a0mapo: ifr.a0mapo,
        },
        HwSm::DdrBulkEn
        | HwSm::GroupAEn
        | HwSm::RsmRstDeassert
        | HwSm::RtcClkWait
        | HwSm::GroupBEn
        | HwSm::GroupCEn
        | HwSm::AssertPwrgood => {
            ringbuf_entry!(Trace::IntermediateHwSmState(hw_sm));
            return;
        }
    };
    ringbuf_entry!(Trace::Diagnosis {
        now,
        reason,
        details
    });
}
