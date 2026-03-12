// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Engine for diagnosing issues with the sequencer failing to reach A0
//!
//! Based on the A0 sequencer fault tree in
//! [Quartz](https://github.com/oxidecomputer/quartz/blob/ndh/a0-fault-tree/hdl/projects/cosmo_seq/sequencer/docs/a0_sequencing_fault_tree.md)

use crate::fmc_sequencer::*;
use ringbuf::{counted_ringbuf, ringbuf_entry};

#[derive(Copy, Clone, PartialEq, counters::Count)]
enum Trace {
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
        now_ms: u64,
        #[count(children)]
        details: Diagnosis,
    },
}
counted_ringbuf!(Trace, 8, Trace::None);

/// Loggable enum explaining a power sequencing failure
#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum Diagnosis {
    StuckInIdle {
        #[count(children)]
        why: WhyStuckInIdle,
        a0_en: bool,
        power_ctrl: PowerCtrlView,
        early_power_rdbks: EarlyPowerRdbksView,
        status: StatusView,
    },
    WaitingForGroupA {
        #[count(children)]
        why: WhyWaitingForGroupA,
        v1p5_rtc: RailStatus,
        v3p3_sp5: RailStatus,
        v1p8_sp5: RailStatus,
    },
    WaitingForSlpCheckpoint {
        #[count(children)]
        why: WhyWaitingForSlpCheckpoint,
        sp5_readbacks: Sp5ReadbacksView,
        ddr5_abcdef: RailStatus,
        ddr5_ghijkl: RailStatus,
    },
    WaitingForGroupB {
        #[count(children)]
        why: WhyWaitingForGroupB,
        v1p1_sp5: RailStatus,
    },
    WaitingForGroupC {
        #[count(children)]
        why: WhyWaitingForGroupC,
        ifr: IfrView,
        vddio_sp5: RailStatus,
        vddcr_cpu0: RailStatus,
        vddcr_cpu1: RailStatus,
        vddcr_soc: RailStatus,
    },
    WaitingForPowerOk {
        #[count(children)]
        why: WhyWaitingForPowerOk,
        rail_pgs: RailPgsView,
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
        ifr: IfrView,
        rail_pgs: RailPgsView,
        rail_pgs_max_hold: RailPgsMaxHoldView,
        rail_enables: RailEnablesView,
        early_power_rdbks: EarlyPowerRdbksView,
    },
    SoftwareDisable {
        a0_sm: seq_api_status::A0Sm,
        a0_en: bool,
        a0mapo: bool,
    },
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum WhyStuckInIdle {
    FanHscNotPg(FanHsc),
    FanPowerNotOk,
    Unknown,
}

#[derive(Copy, Clone, PartialEq)]
pub(crate) struct RailStatus {
    enabled: bool,
    power_good: bool,
    power_good_max_hold: bool,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum RailIssue {
    RailNotEnabled,
    RailNotPowerGood,
    RailPowerGoodIntermittent,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum WhyWaitingForGroupA {
    RailIssue(#[count(children)] RailIssue, GroupARail),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum WhyWaitingForSlpCheckpoint {
    Sp5StuckInS5Sleep,
    Sp5StuckInS3Sleep,
    RsmRstLNotReleased,
    PowerButtonStillAsserted,
    RailIssue(#[count(children)] RailIssue, Ddr5HscRail),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum WhyWaitingForGroupB {
    RailIssue(#[count(children)] RailIssue, GroupBRail),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum WhyWaitingForGroupC {
    RailIssue(#[count(children)] RailIssue, GroupCRail),
    VrControllerAlert(u8),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum WhyWaitingForPowerOk {
    Sp5NotAssertingPowerOk,
    FpgaNotDrivingPowerGood,
    RailIssue(#[count(children)] RailIssue),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
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
    FanHscNotPg(FanHsc),
    VrControllerAlert(u8),
    Unknown,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
#[allow(non_camel_case_types)]
pub(crate) enum GroupARail {
    V1P5_RTC,
    V3P3_SP5_A1,
    V1P8_SP5_A1,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
#[allow(non_camel_case_types)]
pub(crate) enum GroupBRail {
    V1P1_SP5,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
#[allow(non_camel_case_types)]
pub(crate) enum Ddr5HscRail {
    DDR5_ABCDEF,
    DDR5_GHIJKL,
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
#[allow(non_camel_case_types)]
pub(crate) enum GroupCRail {
    VDDIO_SP5_A0,
    VDDCR_CPU0,
    VDDCR_CPU1,
    VDDCR_SOC,
}

fn get_rail_issue<T: Copy>(
    rails: &[(RailStatus, T)],
) -> Option<(RailIssue, T)> {
    if let Some((_r, t)) = rails.iter().find(|(r, _)| !r.enabled) {
        Some((RailIssue::RailNotEnabled, *t))
    } else if let Some((_r, t)) = rails
        .iter()
        .find(|(r, _)| !r.power_good && r.power_good_max_hold)
    {
        Some((RailIssue::RailPowerGoodIntermittent, *t))
    } else if let Some((_r, t)) = rails.iter().find(|(r, _)| !r.power_good) {
        Some((RailIssue::RailNotPowerGood, *t))
    } else {
        None
    }
}

/// Diagnoses a problem with the sequencer failing to get to A0
///
/// The result is logged in a ringbuf
pub(crate) fn run(seq: &Sequencer) {
    let seq_raw_status = SeqRawStatusView::from(&seq.seq_raw_status);
    let seq_api_status = SeqApiStatusView::from(&seq.seq_api_status);
    let power_ctrl = PowerCtrlView::from(&seq.power_ctrl);
    let early_power_rdbks = EarlyPowerRdbksView::from(&seq.early_power_rdbks);
    let status = StatusView::from(&seq.status);
    let rail_enables = RailEnablesView::from(&seq.rail_enables);
    let rail_pgs = RailPgsView::from(&seq.rail_pgs);
    let rail_pgs_max_hold = RailPgsMaxHoldView::from(&seq.rail_pgs_max_hold);
    let sp5_readbacks = Sp5ReadbacksView::from(&seq.sp5_readbacks);
    let debug_enables = DebugEnablesView::from(&seq.debug_enables);
    let ifr = IfrView::from(&seq.ifr);

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
                    } else if ifr.pwr_cont1_to_fpga1_alert {
                        WhyMapo::VrControllerAlert(1)
                    } else if ifr.pwr_cont2_to_fpga1_alert {
                        WhyMapo::VrControllerAlert(2)
                    } else if ifr.pwr_cont3_to_fpga1_alert {
                        WhyMapo::VrControllerAlert(3)
                    } else {
                        WhyMapo::Unknown
                    };
                    Diagnosis::Mapo {
                        why,
                        ifr,
                        rail_pgs,
                        rail_pgs_max_hold,
                        rail_enables,
                        early_power_rdbks,
                    }
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
                    } else {
                        WhyStuckInIdle::Unknown
                    };
                    Diagnosis::StuckInIdle {
                        why,
                        a0_en: power_ctrl.a0_en,
                        power_ctrl,
                        early_power_rdbks,
                        status,
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
            Diagnosis::WaitingForGroupA {
                why: ri
                    .map(|(i, r)| WhyWaitingForGroupA::RailIssue(i, r))
                    .unwrap_or(WhyWaitingForGroupA::Unknown),
                v1p5_rtc,
                v3p3_sp5,
                v1p8_sp5,
            }
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
            Diagnosis::WaitingForSlpCheckpoint {
                why,
                sp5_readbacks,
                ddr5_abcdef,
                ddr5_ghijkl,
            }
        }
        HwSm::GroupBPgAndWait => {
            let (v1p1_sp5,) = rail_status!(rail_state, (v1p1_sp5));
            let why = get_rail_issue(&[(v1p1_sp5, GroupBRail::V1P1_SP5)])
                .map(|(i, r)| WhyWaitingForGroupB::RailIssue(i, r))
                .unwrap_or(WhyWaitingForGroupB::Unknown);
            Diagnosis::WaitingForGroupB { why, v1p1_sp5 }
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
            Diagnosis::WaitingForGroupC {
                why,
                ifr,
                vddio_sp5,
                vddcr_cpu0,
                vddcr_cpu1,
                vddcr_soc,
            }
        }
        HwSm::WaitPwrok => {
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
            } else if let Some((issue, ())) = get_rail_issue(&[
                (v1p5_rtc, ()),
                (v3p3_sp5, ()),
                (v1p8_sp5, ()),
                (v1p1_sp5, ()),
                (vddio_sp5, ()),
                (vddcr_cpu0, ()),
                (vddcr_cpu1, ()),
                (vddcr_soc, ()),
            ]) {
                WhyWaitingForPowerOk::RailIssue(issue)
            } else {
                WhyWaitingForPowerOk::Unknown
            };
            Diagnosis::WaitingForPowerOk {
                why,
                rail_pgs,
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
    let now_ms = userlib::sys_get_timer().now;
    ringbuf_entry!(Trace::Diagnosis { now_ms, details });
}
