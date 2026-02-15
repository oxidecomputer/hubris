// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Grapefruit FPGA process.

#![no_std]
#![no_main]

use drv_cpu_seq_api::{
    PowerState, SeqError as CpuSeqError, StateChangeReason, Transition,
};
use drv_ice40_spi_program as ice40;
use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};
use drv_spartan7_loader_api::Spartan7Loader;
use drv_spi_api::{SpiDevice, SpiServer};
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use fixedstr::FixedStr;
use fmc_sequencer::{nic_api_status, seq_api_status};
use idol_runtime::{NotificationHandler, RequestError};
use task_jefe_api::Jefe;
use userlib::{
    hl, set_timer_relative, sys_get_timer, sys_recv_notification, task_slot,
    RecvMessage,
};

use drv_hf_api::HostFlash;
use ringbuf::{counted_ringbuf, ringbuf_entry, Count};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

mod vcore;
use vcore::VCore;

task_slot!(JEFE, jefe);
task_slot!(LOADER, spartan7_loader);
task_slot!(HF, hf);
task_slot!(SYS, sys);
task_slot!(SPI_FRONT, spi_front);
task_slot!(AUXFLASH, auxflash);
task_slot!(I2C, i2c_driver);
task_slot!(PACKRAT, packrat);

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    #[count(skip)]
    None,
    FpgaInit,
    StartFailed(#[count(children)] SeqError),
    ContinueBitstreamLoad(usize),
    WaitForDone,
    Programmed,

    Startup {
        early_power_rdbks: fmc_sequencer::EarlyPowerRdbksView,
    },
    RegStateValues {
        seq_api_status: fmc_sequencer::SeqApiStatusView,
        seq_raw_status: fmc_sequencer::SeqRawStatusView,
        nic_api_status: fmc_sequencer::NicApiStatusView,
        nic_raw_status: fmc_sequencer::NicRawStatusView,
    },
    RegPgValues {
        rail_pgs: fmc_sequencer::RailPgsView,
        rail_pgs_max_hold: fmc_sequencer::RailPgsMaxHoldView,
    },
    SetState {
        prev: Option<PowerState>,
        next: PowerState,
        #[count(children)]
        why: StateChangeReason,
        now: u64,
    },
    UnexpectedPowerOff {
        our_state: PowerState,
        seq_state: Result<seq_api_status::A0Sm, u8>,
    },
    SequencerInterrupt {
        our_state: PowerState,
        seq_state: Result<seq_api_status::A0Sm, u8>,
    },
    // It's not particularly useful to count this...
    #[count(skip)]
    SequencerIfr(fmc_sequencer::IfrView),
    PowerDownError(drv_cpu_seq_api::SeqError),
    Coretype {
        coretype0: bool,
        coretype1: bool,
        coretype2: bool,
        sp5r1: bool,
        sp5r2: bool,
        sp5r3: bool,
        sp5r4: bool,
    },
    ResetCounts {
        rstn: u8,
        pwrokn: u8,
    },
    Thermtrip,
    A0MapoInterrupt,
    NicMapoInterrupt,
    SmerrInterrupt,
    PmbusAlert {
        now: u64,
    },
    UnexpectedInterrupt,
    CPUPresent(bool),
    EreportSent(usize),
    EreportLost(usize, task_packrat_api::EreportWriteError),
    EreportTooBig,
}
counted_ringbuf!(Trace, 128, Trace::None);

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq, Count)]
enum SeqError {
    AuxFlashError(#[count(children)] drv_auxflash_api::AuxFlashError),
    AuxChecksumMismatch,
    Ice40(#[count(children)] ice40::Ice40Error),
}

impl From<drv_auxflash_api::AuxFlashError> for SeqError {
    fn from(v: drv_auxflash_api::AuxFlashError) -> Self {
        SeqError::AuxFlashError(v)
    }
}

////////////////////////////////////////////////////////////////////////////////

const SP_TO_SP5_NMI_SYNC_FLOOD_L: sys_api::PinSet = sys_api::Port::J.pin(2);
const SP_TO_SP5_PROCHOT_L: sys_api::PinSet = sys_api::Port::H.pin(5);
const SP_CHASSIS_STATUS_LED: sys_api::PinSet = sys_api::Port::C.pin(6);
const SP_TO_FPGA2_SYSTEM_RESET_L: sys_api::PinSet = sys_api::Port::A.pin(5);

// Disabled due to hardware-cosmo#659 (on Cosmo rev A this is PB7, but we need
// to use that pin for FMC).
const SP_TO_IGN_TRGT_FPGA_FAULT_L: Option<sys_api::PinSet> = None;

const SP5_TO_SP_PRESENT_L: sys_api::PinSet = sys_api::Port::C.pin(13);

const SP5_TO_SP_SP5R1: sys_api::PinSet = sys_api::Port::I.pin(4);
const SP5_TO_SP_SP5R2: sys_api::PinSet = sys_api::Port::H.pin(15);
const SP5_TO_SP_SP5R3: sys_api::PinSet = sys_api::Port::F.pin(3);
const SP5_TO_SP_SP5R4: sys_api::PinSet = sys_api::Port::F.pin(4);

const SP5_TO_SP_CORETYPE0: sys_api::PinSet = sys_api::Port::I.pin(5);
const SP5_TO_SP_CORETYPE1: sys_api::PinSet = sys_api::Port::I.pin(10);
const SP5_TO_SP_CORETYPE2: sys_api::PinSet = sys_api::Port::I.pin(11);

// All of these are externally pulled to V3P3_SP5_A1
const CORETYPE_PULL: sys_api::Pull = sys_api::Pull::None;
const CPU_PRESENT_L_PULL: sys_api::Pull = sys_api::Pull::None;
const SP5R1_PULL: sys_api::Pull = sys_api::Pull::None;
const SP5R2_PULL: sys_api::Pull = sys_api::Pull::None;
const SP5R3_PULL: sys_api::Pull = sys_api::Pull::None;
const SP5R4_PULL: sys_api::Pull = sys_api::Pull::None;

use gpio_irq_pins::SEQ_IRQ;

////////////////////////////////////////////////////////////////////////////////

/// Helper type which includes both sequencer and NIC state machine states
struct StateMachineStates {
    seq: Result<seq_api_status::A0Sm, u8>,
    nic: Result<nic_api_status::NicSm, u8>,
}

const EREPORT_BUF_LEN: usize = microcbor::max_cbor_len_for!(
    task_packrat_api::Ereport<EreportClass, EreportKind>,
);

#[export_name = "main"]
fn main() -> ! {
    // Populate packrat with our mac address and identity.
    let packrat = Packrat::from(PACKRAT.get_task_id());
    read_vpd_and_load_packrat(&packrat, I2C.get_task_id());

    let ereport_buf = {
        use static_cell::ClaimOnceCell;
        static EREPORT_BUF: ClaimOnceCell<[u8; EREPORT_BUF_LEN]> =
            ClaimOnceCell::new([0; EREPORT_BUF_LEN]);
        EREPORT_BUF.claim()
    };

    //
    // Apply the configuration mitigation on the BMR491, if required. This
    // is an external device access and may fail. We'll attempt it thrice
    // and then allow boot to continue.
    //
    {
        use drv_i2c_devices::bmr491::{Bmr491, ExternalInputVoltageProtection};

        let dev = i2c_config::devices::bmr491_u80(I2C.get_task_id());
        let driver = Bmr491::new(&dev, 0);

        // Cosmo provides external undervoltage protection that kicks in at a
        // lower voltage than we'd like to tolerate, so, request additional
        // protection from the mitigation code.
        let protection = ExternalInputVoltageProtection::CutoffBelow40V;

        let (failures, last_cause, succeeded) =
            match driver.apply_mitigation_for_rma2402311(protection) {
                Ok(r) => (r.failures, r.last_failure, true),
                Err(e) => (e.retries, Some(e.last_cause), false),
            };

        if let Some(last_cause) = last_cause {
            // Report the failure even if we eventually succeeded.
            try_send_ereport(
                &packrat,
                &mut ereport_buf[..],
                EreportClass::Bmr491MitigationFailure,
                EreportKind::Bmr491MitigationFailure {
                    refdes: FixedStr::from_str(dev.component_id()),
                    failures,
                    last_cause,
                    succeeded,
                },
            );
        }
    }

    match init(packrat, ereport_buf) {
        // Set up everything nicely, time to start serving incoming messages.
        Ok(mut server) => {
            // Enable the backplane PCIe clock if requested
            if cfg!(feature = "enable-backplane-pcie-clk") {
                server.seq.pcie_clk_ctrl.modify(|p| p.set_clk_en(true));
            }
            // Power on, unless suppressed by the `stay-in-a2` feature
            if !cfg!(feature = "stay-in-a2") {
                _ = server.set_state_impl(
                    PowerState::A0,
                    StateChangeReason::InitialPowerOn,
                );
            }

            let mut buffer = [0; idl::INCOMING_SIZE];
            loop {
                idol_runtime::dispatch(&mut buffer, &mut server);
            }
        }

        // Initializing the sequencer failed.
        Err(e) => {
            // Tell everyone that something's broken, as loudly as possible.
            ringbuf_entry!(Trace::StartFailed(e));
            // Leave FAULT_PIN_L low (which is done at the start of init)

            // All these moments will be lost in time, like tears in rain...
            // Time to die.
            loop {
                // Sleeping with all bits in the notification mask clear means
                // we should never be notified --- and if one never wakes up,
                // the difference between sleeping and dying seems kind of
                // irrelevant. But, `rustc` doesn't realize that this should
                // never return, we'll stick it in a `loop` anyway so the main
                // function can return `!`
                sys_recv_notification(0);
            }
        }
    }
}

fn init(
    packrat: Packrat,
    ereport_buf: &'static mut [u8; EREPORT_BUF_LEN],
) -> Result<ServerImpl, SeqError> {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    // Pull the fault line low while we're loading
    if let Some(pin) = SP_TO_IGN_TRGT_FPGA_FAULT_L {
        sys.gpio_configure_output(
            pin,
            sys_api::OutputType::OpenDrain,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );
        sys.gpio_reset(pin);
    }

    // Turn off the chassis LED, in case this is a task restart (and not a
    // full chip restart, which would leave the GPIO unconfigured).
    sys.gpio_configure_output(
        SP_CHASSIS_STATUS_LED,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );
    sys.gpio_reset(SP_CHASSIS_STATUS_LED);

    // Set all of the presence-related pins to be inputs
    sys.gpio_configure_input(SP5_TO_SP_CORETYPE0, CORETYPE_PULL);
    sys.gpio_configure_input(SP5_TO_SP_CORETYPE1, CORETYPE_PULL);
    sys.gpio_configure_input(SP5_TO_SP_CORETYPE2, CORETYPE_PULL);
    sys.gpio_configure_input(SP5_TO_SP_PRESENT_L, CPU_PRESENT_L_PULL);
    sys.gpio_configure_input(SP5_TO_SP_SP5R1, SP5R1_PULL);
    sys.gpio_configure_input(SP5_TO_SP_SP5R2, SP5R2_PULL);
    sys.gpio_configure_input(SP5_TO_SP_SP5R3, SP5R3_PULL);
    sys.gpio_configure_input(SP5_TO_SP_SP5R4, SP5R4_PULL);

    // Sequencer interrupt
    sys.gpio_configure_input(SEQ_IRQ, sys_api::Pull::None);
    sys.gpio_irq_configure(notifications::SEQ_IRQ_MASK, sys_api::Edge::Falling);

    let spi_front = drv_spi_api::Spi::from(SPI_FRONT.get_task_id());
    let aux = drv_auxflash_api::AuxFlash::from(AUXFLASH.get_task_id());

    // Hold the ice40 in reset
    let config = ice40::Config {
        creset: sys_api::Port::A.pin(4),
        cdone: sys_api::Port::A.pin(3),
    };
    preinit_front_fpga(&sys, &config);

    // Wait for the Spartan-7 to be loaded, then update its checksum registers
    let loader = Spartan7Loader::from(LOADER.get_task_id());

    // Set up the checksum registers for the Spartan7 FPGA
    let token = loader.get_token();
    let info = fmc_periph::info::Info::new(token);
    let short_checksum = gen::SPARTAN7_FPGA_BITSTREAM_CHECKSUM[..4]
        .try_into()
        .unwrap();
    info.fpga_checksum
        .modify(|r| r.set_data(u32::from_be_bytes(short_checksum)));

    init_front_fpga(
        &sys,
        &spi_front.device(drv_spi_api::devices::MUX),
        &aux,
        &config,
    )?;

    // Bring up the SP5 NMI and PROCHOT pins
    sys.gpio_set(SP_TO_SP5_NMI_SYNC_FLOOD_L);
    sys.gpio_configure_output(
        SP_TO_SP5_NMI_SYNC_FLOOD_L,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );
    sys.gpio_set(SP_TO_SP5_PROCHOT_L);
    sys.gpio_configure_output(
        SP_TO_SP5_PROCHOT_L,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );

    // Clear the fault pin
    if let Some(pin) = SP_TO_IGN_TRGT_FPGA_FAULT_L {
        sys.gpio_set(pin);
    }

    // Turn on the chassis LED!
    sys.gpio_set(SP_CHASSIS_STATUS_LED);

    Ok(ServerImpl::new(loader, packrat, ereport_buf))
}

/// Configures the front FPGA pins and holds it in reset
fn preinit_front_fpga(sys: &sys_api::Sys, config: &ice40::Config) {
    // Make the user reset pin a low output
    sys.gpio_reset(SP_TO_FPGA2_SYSTEM_RESET_L);
    sys.gpio_configure_output(
        config.creset,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );

    ice40::configure_pins(sys, config);

    // This is also called in `ice40::begin_bitstream_load`, but we're going to
    // wait for the sequencer to be loaded first, and want this to be in reset
    // while we're waiting.
    sys.gpio_reset(config.creset);
}

/// Initialize the front FPGA, which is an ICE40
fn init_front_fpga<S: SpiServer>(
    sys: &sys_api::Sys,
    dev: &SpiDevice<S>,
    aux: &drv_auxflash_api::AuxFlash,
    config: &ice40::Config,
) -> Result<(), SeqError> {
    ringbuf_entry!(Trace::FpgaInit);

    ice40::begin_bitstream_load(dev, sys, config).map_err(SeqError::Ice40)?;

    let r = aux.get_compressed_blob_streaming(
        *b"ICE4",
        |chunk| -> Result<(), SeqError> {
            ice40::continue_bitstream_load(dev, chunk)
                .map_err(|e| SeqError::Ice40(ice40::Ice40Error::Spi(e)))?;
            ringbuf_entry!(Trace::ContinueBitstreamLoad(chunk.len()));
            Ok(())
        },
    );
    let sha_out = match r {
        Ok(s) => s,
        Err(e) => {
            let _ = dev.release();
            return Err(e);
        }
    };

    if sha_out != gen::FRONT_FPGA_BITSTREAM_CHECKSUM {
        // Drop the device into reset and hold it there
        sys.gpio_reset(config.creset);
        hl::sleep_for(1);
        let _ = dev.release();

        return Err(SeqError::AuxChecksumMismatch);
    }

    ringbuf_entry!(Trace::WaitForDone);
    ice40::finish_bitstream_load(dev, sys, config).map_err(SeqError::Ice40)?;
    ringbuf_entry!(Trace::Programmed);

    // Bring the user design out of reset
    sys.gpio_set(SP_TO_FPGA2_SYSTEM_RESET_L);

    Ok(())
}

////////////////////////////////////////////////////////////////////////////////

#[allow(unused)]
struct ServerImpl {
    state: PowerState,
    jefe: Jefe,
    sys: Sys,
    hf: HostFlash,
    seq: fmc_sequencer::Sequencer,
    espi: fmc_periph::espi::Espi,
    debug: fmc_periph::debug_ctrl::DebugCtrl,
    vcore: VCore,
    /// Static buffer for encoding ereports. This is a static so that we don't
    /// have it on the stack when encoding ereports.
    ereport_buf: &'static mut [u8; EREPORT_BUF_LEN],
}

#[derive(microcbor::Encode)]
pub enum EreportClass {
    #[cbor(rename = "hw.pwr.pmbus.alert")]
    PmbusAlert,
    #[cbor(rename = "hw.pwr.bmr491.mitfail")]
    Bmr491MitigationFailure,
}

#[derive(microcbor::EncodeFields)]
pub(crate) enum EreportKind {
    Bmr491MitigationFailure {
        refdes: FixedStr<'static, { crate::i2c_config::MAX_COMPONENT_ID_LEN }>,
        failures: u32,
        last_cause: drv_i2c_devices::bmr491::MitigationFailureKind,
        succeeded: bool,
    },
    PmbusAlert {
        refdes: FixedStr<'static, { crate::i2c_config::MAX_COMPONENT_ID_LEN }>,
        rail: vcore::Rail,
        time: u64,
        pwr_good: Option<bool>,
        pmbus_status: PmbusStatus,
    },
}

#[derive(Copy, Clone, Default, microcbor::Encode)]
pub(crate) struct PmbusStatus {
    word: Option<u16>,
    input: Option<u8>,
    iout: Option<u8>,
    vout: Option<u8>,
    temp: Option<u8>,
    cml: Option<u8>,
    mfr: Option<u8>,
}

impl ServerImpl {
    fn new(
        loader: drv_spartan7_loader_api::Spartan7Loader,
        packrat: Packrat,
        ereport_buf: &'static mut [u8; EREPORT_BUF_LEN],
    ) -> Self {
        let now = sys_get_timer().now;

        let seq = fmc_sequencer::Sequencer::new(loader.get_token());
        let espi = fmc_periph::espi::Espi::new(loader.get_token());
        let debug = fmc_periph::debug_ctrl::DebugCtrl::new(loader.get_token());

        ringbuf_entry!(Trace::Startup {
            early_power_rdbks: (&seq.early_power_rdbks).into(),
        });
        ringbuf_entry!(Trace::SetState {
            prev: None, // dummy value
            next: PowerState::A2,
            why: StateChangeReason::InitialPowerOn,
            now,
        });
        let jefe = Jefe::from(JEFE.get_task_id());
        jefe.set_state(PowerState::A2 as u32);

        ServerImpl {
            state: PowerState::A2,
            jefe,
            sys: Sys::from(SYS.get_task_id()),
            hf: HostFlash::from(HF.get_task_id()),
            seq,
            espi,
            debug,
            vcore: VCore::new(I2C.get_task_id(), packrat),
            ereport_buf,
        }
    }

    fn get_state_impl(&self) -> PowerState {
        self.state
    }

    /// Logs a set of state registers, returning the state machine states
    fn log_state_registers(&self) -> StateMachineStates {
        let seq_api_status = (&self.seq.seq_api_status).into();
        let nic_api_status = (&self.seq.nic_api_status).into();

        ringbuf_entry!(Trace::RegStateValues {
            seq_api_status,
            seq_raw_status: (&self.seq.seq_raw_status).into(),
            nic_api_status,
            nic_raw_status: (&self.seq.nic_raw_status).into(),
        });
        StateMachineStates {
            seq: seq_api_status.a0_sm,
            nic: nic_api_status.nic_sm,
        }
    }

    /// Logs a set of power good registers
    fn log_pg_registers(&self) {
        ringbuf_entry!(Trace::RegPgValues {
            rail_pgs: (&self.seq.rail_pgs).into(),
            rail_pgs_max_hold: (&self.seq.rail_pgs_max_hold).into(),
        });
    }

    fn set_state_impl(
        &mut self,
        state: PowerState,
        why: StateChangeReason,
    ) -> Result<Transition, CpuSeqError> {
        let now = sys_get_timer().now;
        ringbuf_entry!(Trace::SetState {
            prev: Some(self.state),
            next: state,
            why,
            now,
        });

        use seq_api_status::A0Sm;
        match (self.get_state_impl(), state) {
            (PowerState::A2, PowerState::A0) => {
                // Reset edge counters in the sequencer
                self.seq.amd_reset_fedges.set_counts(0);
                self.seq.amd_pwrok_fedges.set_counts(0);

                // Reset edge interrupts flags
                self.seq.ifr.modify(|h| {
                    h.set_amd_pwrok_fedge(false);
                    h.set_amd_rstn_fedge(false);
                });

                // Tell the sequencer to go to A0
                self.seq.power_ctrl.modify(|m| m.set_a0_en(true));

                // Wait 2 seconds for power-up
                let mut okay = false;
                let mut err = CpuSeqError::A0Timeout;
                for _ in 0..200 {
                    let state = self.log_state_registers();
                    match state.seq {
                        Ok(A0Sm::Done) => {
                            okay = true;
                            break;
                        }
                        Ok(A0Sm::Faulted) | Err(_) => {
                            break;
                        }
                        Ok(A0Sm::EnableGrpA) => {
                            // hardware-cosmo#658 prevents us from checking `CPU_PRESENT`
                            // at `A0Sm::ENABLE_GRP_A` time on rev-a boards
                            if cfg!(target_board = "cosmo-a") {
                                ringbuf_entry!(Trace::CPUPresent(true));
                            } else {
                                let present =
                                    self.sys.gpio_read(SP5_TO_SP_PRESENT_L)
                                        == 0;
                                ringbuf_entry!(Trace::CPUPresent(present));

                                if !present {
                                    err = CpuSeqError::CPUNotPresent;
                                    break;
                                }
                            }
                        }
                        _ => (),
                    }
                    hl::sleep_for(10);
                }

                if !okay {
                    // We'll return to A2, leaving jefe and our local state
                    // unchanged (since they're set after this block).
                    self.log_state_registers();
                    self.log_pg_registers();
                    self.seq.power_ctrl.modify(|m| m.set_a0_en(false));

                    return Err(err);
                }

                let coretype0 = self.sys.gpio_read(SP5_TO_SP_CORETYPE0) != 0;
                let coretype1 = self.sys.gpio_read(SP5_TO_SP_CORETYPE1) != 0;
                let coretype2 = self.sys.gpio_read(SP5_TO_SP_CORETYPE2) != 0;
                let sp5r1 = self.sys.gpio_read(SP5_TO_SP_SP5R1) != 0;
                let sp5r2 = self.sys.gpio_read(SP5_TO_SP_SP5R2) != 0;
                let sp5r3 = self.sys.gpio_read(SP5_TO_SP_SP5R3) != 0;
                let sp5r4 = self.sys.gpio_read(SP5_TO_SP_SP5R4) != 0;

                ringbuf_entry!(Trace::Coretype {
                    coretype0,
                    coretype1,
                    coretype2,
                    sp5r1,
                    sp5r2,
                    sp5r3,
                    sp5r4
                });

                // From sp5-mobo-guide-56870_1.1.pdf table 72
                match (coretype0, coretype1, coretype2) {
                    // These correspond to Type-2 and Type-3
                    (true, false, true) | (true, false, false) => (),
                    // Reject all other combos and return to A0
                    _ => {
                        self.seq.power_ctrl.modify(|m| m.set_a0_en(false));
                        return Err(CpuSeqError::UnrecognizedCPU);
                    }
                };

                // From sp5-mobo-guide-56870_1.1.pdf table 73
                match (sp5r1, sp5r2, sp5r3, sp5r4) {
                    // There is only combo we accept here
                    (true, false, false, false) => (),
                    // Reject all other combos and return to A0
                    _ => {
                        self.seq.power_ctrl.modify(|m| m.set_a0_en(false));
                        return Err(CpuSeqError::UnrecognizedCPU);
                    }
                };
                // Turn on the voltage regulator undervolt alerts.
                self.enable_sequencer_interrupts();

                // Flip the host flash mux so the CPU can read from it
                // (this is secretly infallible on Cosmo, so we can unwrap it)
                self.hf.set_mux(drv_hf_api::HfMuxState::HostCPU).unwrap();
            }
            (PowerState::A0, PowerState::A2)
            | (PowerState::A0PlusHP, PowerState::A2)
            | (PowerState::A0Thermtrip, PowerState::A2)
            | (PowerState::A0Reset, PowerState::A2) => {
                // Disable our interrupts before we shutdown
                self.disable_sequencer_interrupts();
                self.seq.power_ctrl.modify(|m| m.set_a0_en(false));
                let mut okay = false;
                for _ in 0..200 {
                    let state = self.log_state_registers();
                    match state.seq {
                        Ok(A0Sm::Idle) => {
                            okay = true;
                            break;
                        }
                        Ok(A0Sm::Faulted) | Err(_) => {
                            break;
                        }
                        _ => (),
                    }
                    hl::sleep_for(10);
                }
                if !okay {
                    self.log_state_registers();
                    self.log_pg_registers();
                    // We can't do much else here, since we already cleared the
                    // a0_en flag to disable the sequencer.
                }
                // Flip the host flash mux so the SP can read from it
                // (this is secretly infallible on Cosmo, so we can unwrap it)
                self.hf.set_mux(drv_hf_api::HfMuxState::SP).unwrap();
            }

            // This is purely an accounting change
            (PowerState::A0, PowerState::A0PlusHP) => (),
            // A0PlusHP is a substate of A0; if we are in A0PlusHP and we are
            // asked to go to A0, return `Unchanged`, because `A0PlusHP` means
            // we are already in A0.
            // Similarly, A2PlusFans "counts as" A2 for the purpose of
            // externally-requested transitions.
            (PowerState::A0PlusHP, PowerState::A0)
            | (PowerState::A2PlusFans, PowerState::A2) => {
                return Ok(Transition::Unchanged)
            }
            // If we are already in the requested state, return `Unchanged`.
            (current, requested) if current == requested => {
                return Ok(Transition::Unchanged)
            }

            _ => return Err(CpuSeqError::IllegalTransition),
        }

        self.set_state_internal(state);
        Ok(Transition::Changed)
    }

    /// Updates our internal `state` and the global state in `jefe`
    fn set_state_internal(&mut self, state: PowerState) {
        self.state = state;
        self.jefe.set_state(state as u32);
        self.poke_timer();
    }

    /// Returns the current timer interval, in milliseconds
    ///
    /// If we are in `A0`, then we are waiting for the NIC to come up;
    /// Once we are in `A0PlusHP` we rely on sequencer interrupts for
    /// all our state transitions. We still want to catch an unexpected
    /// case of sequencer failure so poll for that case specifically.
    fn poll_interval(&self) -> Option<u32> {
        match self.state {
            PowerState::A0 => Some(10),
            // we are hoping that a VRM fault will be clearable soon...
            _ if self.vcore.is_still_faulted() => Some(100),
            PowerState::A0PlusHP => Some(1000),
            _ => None,
        }
    }

    /// Updates the system timer
    fn poke_timer(&self) {
        if let Some(interval) = self.poll_interval() {
            set_timer_relative(interval, notifications::TIMER_MASK);
        }
    }

    /// Powers down to A2, if that fails for some reason just
    /// log an error
    fn emergency_a2(&mut self, reason: StateChangeReason) {
        // Power down to A2, updating our internal state.  We can't
        // handle errors here, so log them and continue.
        if let Err(e) = self.set_state_impl(PowerState::A2, reason) {
            ringbuf_entry!(Trace::PowerDownError(e))
        }
    }

    fn enable_sequencer_interrupts(&mut self) {
        // Clear `ifr` in case spurious flags accumulated while disabled
        self.seq.ifr.modify(|m| {
            // IFR flags are write-1-clear.
            m.set_fanfault(true);
            m.set_thermtrip(true);
            m.set_smerr_assert(true);
            m.set_a0mapo(true);
            m.set_nicmapo(true);
            m.set_amd_pwrok_fedge(true);
            m.set_amd_rstn_fedge(true);
        });

        let _ = self.sys.gpio_irq_control(
            notifications::SEQ_IRQ_MASK,
            sys_api::IrqControl::Enable,
        );
        // Enable the undervoltage warning PMBus alert from the Vcore
        // regulators.
        //
        // Yes, we just ignore the error here --- while that seems a bit
        // sketchy, but what else can we do? It seems pretty bad to panic and
        // say "nope, the computer won't turn on" because we weren't able to do
        // an I2C transaction to turn on an interrupt that we only use for
        // monitoring for faults. The initialize method will retry internally a
        // few times, so we should power through any transient I2C messiness,
        // and any I2C errors that occur get logged in the `vcore` module's
        // ringbuf.
        let _ = self.vcore.initialize_uv_warning();
        self.seq.ier.modify(|m| {
            m.set_fanfault(true);
            m.set_thermtrip(true);
            m.set_smerr_assert(true);
            m.set_a0mapo(true);
            m.set_nicmapo(true);
            m.set_amd_pwrok_fedge(true);
            m.set_amd_rstn_fedge(true);
            // PMBus alert bits for Renesas RAA229620A PWM controllers.
            m.set_pwr_cont1_to_fpga1_alert(true);
            m.set_pwr_cont2_to_fpga1_alert(true);
        });
    }

    fn disable_sequencer_interrupts(&mut self) {
        self.seq.ier.modify(|m| {
            m.set_fanfault(false);
            m.set_thermtrip(false);
            m.set_smerr_assert(false);
            m.set_a0mapo(false);
            m.set_nicmapo(false);
            m.set_amd_pwrok_fedge(false);
            m.set_amd_rstn_fedge(false);

            m.set_pwr_cont1_to_fpga1_alert(false);
            m.set_pwr_cont2_to_fpga1_alert(false);
        });
        let _ = self.sys.gpio_irq_control(
            notifications::SEQ_IRQ_MASK,
            sys_api::IrqControl::Disable,
        );
    }

    fn handle_sequencer_interrupt(&mut self) {
        let ifr = self.seq.ifr.view();
        ringbuf_entry!(Trace::SequencerIfr(ifr));
        let now = sys_get_timer().now;

        enum InternalAction {
            Reset,
            NicMapo,
            ThermTrip,
            Smerr,
            Mapo,
            None,
            Unexpected,
        }

        // We check these in lowest to highest priority:
        //
        // 1. PMBus alerts from the VCore voltage regulators are recorded and
        //    produce an ereport, but don't change the power state directly,
        //    as they may just be warnings that don't represent a loss of
        //    power. If a PMBus fault causes the VRM(s) to deassert POWER_GOOD,
        //    that also results in a MAPO from the FPGA, so just seeing the
        //    PMBus alert doesn't transition our state.
        // 2. A NIC MAPO will just transition our state from A0+HP to A0, as
        //    the host is responsible for NIC sequencing. Since other
        //    interrupts we handle will transition us to lower power states,
        //    they have priority over a NIC MAPO that just sends us to A0.
        // 3. We expect the CPU to handle reset nicely, so we just log that.
        // 4. Thermal trip is a terminal state in that we log it but don't
        //    actually make any changes to the sequencer.
        // 5. SMERR is treated as a higher priority than MAPO arbitrarily.
        // we probably(?) won't see multiple of these set at a time but
        // it's important to account for that case;

        let mut action = InternalAction::Unexpected;

        if ifr.pwr_cont1_to_fpga1_alert || ifr.pwr_cont2_to_fpga1_alert {
            // We got a PMBus alert from one of the Vcore regulators.
            //
            // Note that --- unlike other IRQs from the FPGA --- we don't clear
            // the IFR bits for PMALERT interrupts. Unlike the other IRQs, which
            // are either edge-triggered in the FPGA or generated internally by
            // the FPGA, the PMALERT IRQs from the FPGA are level-triggered, and
            // are just passed through from the value of the PMALERT_L pins.
            // They are cleared not by clearing the IFR bits, but by instructing
            // the VRM to clear the PMBus alert, which happens in
            // `self.vcore.handle_pmbus_alert`.
            //
            // See also:
            // https://github.com/oxidecomputer/quartz/blob/bdc5fb31e1905a1b66c19647fe2d156dd1b97b7b/hdl/projects/cosmo_seq/sequencer/sequencer_regs.vhd#L243-L246
            ringbuf_entry!(Trace::PmbusAlert { now });
            let which_vrms = vcore::Vrms {
                pwr_cont1: ifr.pwr_cont1_to_fpga1_alert,
                pwr_cont2: ifr.pwr_cont2_to_fpga1_alert,
            };
            self.vcore
                .handle_pmbus_alert(which_vrms, now, self.ereport_buf);

            // We need not instruct the sequencer to reset. PMBus alerts from
            // the RAA229620As are divided into two categories, "warnings" and
            // "faults", where "warnings" just pull PMALERT_L and set status
            // bits, and "faults" also cause the VRM to deassert POWER_GOOD. If
            // POWER_GOOD is deasserted, the sequencer FPGA will notice that and
            // generate a subsequent IRQ, which is handled separately. So, all
            // we need to do here is proceed and handle any other interrupts.
            //
            // However, the only way to make the pins deassert (and thus, the
            // IRQ go away) is to clear the faults in the regulator.
            // N.B.: unlike other FPGA sequencer alerts, we cannot clear the
            // IFR bits for these; they are hot as long as the PMALERT pin from
            // the RAA229620As is asserted.
            //
            // Per the RAA229620A datasheet (R16DS0309EU0200 Rev.2.00, page 36),
            // clearing the fault in the regulator will deassert PMALERT_L,
            // releasing the IRQ, but the fault bits to be reset if the fault
            // condition still exists. This means that if the fault condition
            // has not cleared yet, the VRM will just immediately reassert
            // PMALERT_L. Therefore, if we have an ongoing fault condition, we
            // will mask out the IER bits for the whichever VRM(s) are presently
            // asserting PMALERT_L, and continue trying to clear the fault in
            // the timer loop. If the fault clears, we shall then re-enable
            // interrupts for those VRMs.
            //
            // The `vcore` module tells us whether any faults have successfully
            // cleared. Set the IER bits based on that.
            let vcore::Vrms {
                pwr_cont1,
                pwr_cont2,
            } = self.vcore.can_we_unmask_any_vrm_irqs_again();
            self.seq.ier.modify(|ier| {
                ier.set_pwr_cont1_to_fpga1_alert(pwr_cont1);
                ier.set_pwr_cont2_to_fpga1_alert(pwr_cont2);
            });

            // Nothing else need be done unles other IRQs have also fired.
            action = InternalAction::None;
        }

        if ifr.amd_pwrok_fedge || ifr.amd_rstn_fedge {
            let rstn = self.seq.amd_reset_fedges.counts();
            let pwrokn = self.seq.amd_pwrok_fedges.counts();

            // counters and ifr are cleared in the A2 -> A0 transition
            // host_sp_comms will be notified of this change and will
            // call back into this task to reboot the system (going to
            // A2 then back into A0)
            ringbuf_entry!(Trace::ResetCounts { rstn, pwrokn });
            action = InternalAction::Reset;
        }

        if ifr.nicmapo {
            self.seq.ifr.modify(|h| h.set_nicmapo(true));
            ringbuf_entry!(Trace::NicMapoInterrupt);
            action = InternalAction::NicMapo;
            // TODO(eliza): ereport!!!
        }

        if ifr.thermtrip {
            self.seq.ifr.modify(|h| h.set_thermtrip(true));
            ringbuf_entry!(Trace::Thermtrip);
            action = InternalAction::ThermTrip;
            // Great place for an ereport?
        }

        if ifr.a0mapo {
            self.log_pg_registers();
            self.seq.ifr.modify(|h| h.set_a0mapo(true));
            ringbuf_entry!(Trace::A0MapoInterrupt);
            action = InternalAction::Mapo;
            // Great place for an ereport?
        }

        if ifr.smerr_assert {
            self.seq.ifr.modify(|h| h.set_smerr_assert(true));
            ringbuf_entry!(Trace::SmerrInterrupt);
            action = InternalAction::Smerr;
            // Great place for an ereport?
        }
        // Fan Fault is unconnected

        match action {
            InternalAction::Reset => {
                // host_sp_comms will be notified of this change and will
                // call back into this task to reboot the system (going to
                // A2 then back into A0)
                ringbuf_entry!(Trace::SetState {
                    prev: self.state(),
                    next: PowerState::A0Reset,
                    why: StateChangeReason::CpuReset,
                    now,
                });
                self.set_state_internal(PowerState::A0Reset);
            }
            InternalAction::NicMapo => {
                // Presumably we are in A0+HP, so send us back to A0 so that the
                // thermal loop will stop trying to talk to the NIC, and hope
                // the host resequences it.
                ringbuf_entry!(Trace::SetState {
                    prev: self.state(),
                    next: PowerState::A0,
                    why: StateChangeReason::NicMapo,
                    now,
                });
                self.set_state_internal(PowerState::A0);
            }
            InternalAction::ThermTrip => {
                // This is a terminal state; we set our state to `A0Thermtrip`
                // but do not expect any other task to take action right now
                ringbuf_entry!(Trace::SetState {
                    prev: self.state(),
                    next: PowerState::A0Thermtrip,
                    why: StateChangeReason::ThermTrip,
                    now,
                });
                self.set_state_internal(PowerState::A0Thermtrip);
            }
            InternalAction::Mapo => {
                // This is a terminal state (for now)
                self.emergency_a2(StateChangeReason::A0Mapo);
            }
            InternalAction::Smerr => {
                // This is a terminal state (for now)
                self.emergency_a2(StateChangeReason::SmerrAssert);
            }
            InternalAction::None => {
                // That's right, just do nothing.
            }
            InternalAction::Unexpected => {
                // This is unexpected, logging is the best we can do
                ringbuf_entry!(Trace::UnexpectedInterrupt);
            }
        };
    }
}

impl idl::InOrderSequencerImpl for ServerImpl {
    fn get_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<PowerState, RequestError<core::convert::Infallible>> {
        Ok(self.get_state_impl())
    }

    fn set_state(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
    ) -> Result<Transition, RequestError<CpuSeqError>> {
        self.set_state_impl(state, StateChangeReason::Other)
            .map_err(Into::into)
    }

    fn set_state_with_reason(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
        reason: StateChangeReason,
    ) -> Result<Transition, RequestError<CpuSeqError>> {
        self.set_state_impl(state, reason).map_err(Into::into)
    }

    fn send_hardware_nmi(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        // The required length for an NMI pulse is apparently not documented.
        //
        // Let's try 25 ms!
        self.sys.gpio_reset(SP_TO_SP5_NMI_SYNC_FLOOD_L);
        hl::sleep_for(25);
        self.sys.gpio_set(SP_TO_SP5_NMI_SYNC_FLOOD_L);
        Ok(())
    }

    fn read_fpga_regs(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 64], RequestError<core::convert::Infallible>> {
        Err(RequestError::Fail(
            idol_runtime::ClientError::BadMessageContents,
        ))
    }

    fn last_post_code(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<core::convert::Infallible>> {
        Ok(self.espi.last_post_code.payload())
    }

    fn gpio_edge_count(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<core::convert::Infallible>> {
        Ok(self.debug.sp5_dbg2_toggle_counter.cnts())
    }

    fn gpio_cycle_count(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<core::convert::Infallible>> {
        Ok(self.debug.sp5_dbg2_toggle_timer.cnts())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK | notifications::SEQ_IRQ_MASK
    }

    fn handle_notification(&mut self, bits: userlib::NotificationBits) {
        if bits.check_notification_mask(notifications::SEQ_IRQ_MASK) {
            let state = self.log_state_registers();
            ringbuf_entry!(Trace::SequencerInterrupt {
                our_state: self.state,
                seq_state: state.seq,
            });

            // Read the IFR register and handle any pending interrupts until the
            // SEQ_IRQ signal is deasserted (it's active low).
            while self.sys.gpio_read(SEQ_IRQ) == 0 {
                self.handle_sequencer_interrupt();
            }
        }

        if !bits.has_timer_fired(notifications::TIMER_MASK) {
            return;
        }
        let state = self.log_state_registers();
        use fmc_sequencer::{nic_api_status::NicSm, seq_api_status::A0Sm};

        // Detect when the NIC comes online
        // TODO: should we handle the NIC powering down while the main CPU
        // power remains up?
        if self.state == PowerState::A0 && state.nic == Ok(NicSm::Done) {
            self.set_state_impl(
                PowerState::A0PlusHP,
                StateChangeReason::InitialPowerOn,
            )
            .unwrap(); // this should be infallible
        }

        if self.vcore.is_still_faulted() {
            let vcore::Vrms {
                pwr_cont1,
                pwr_cont2,
            } = self.vcore.can_we_unmask_any_vrm_irqs_again(); // ...please?

            // okay, great!
            self.seq.ier.modify(|ier| {
                ier.set_pwr_cont1_to_fpga1_alert(pwr_cont1);
                ier.set_pwr_cont2_to_fpga1_alert(pwr_cont2);
            });
        }

        // If Hubris thinks the system is up, do some basic checks
        if matches!(self.state, PowerState::A0 | PowerState::A0PlusHP) {
            // Detect the FPGA powering off without us
            if state.seq != Ok(A0Sm::Done) {
                ringbuf_entry!(Trace::UnexpectedPowerOff {
                    our_state: self.state,
                    seq_state: state.seq,
                });
                self.log_pg_registers();

                self.emergency_a2(StateChangeReason::Unknown);
            }
        }

        self.poke_timer();
    }
}

fn try_send_ereport(
    packrat: &task_packrat_api::Packrat,
    ereport_buf: &mut [u8],
    class: EreportClass,
    report: EreportKind,
) {
    let eresult = packrat.deliver_microcbor_ereport(
        &task_packrat_api::Ereport {
            class,
            version: 0,
            report,
        },
        ereport_buf,
    );
    match eresult {
        Ok(len) => ringbuf_entry!(Trace::EreportSent(len)),
        Err(task_packrat_api::EreportEncodeError::Packrat { len, err }) => {
            ringbuf_entry!(Trace::EreportLost(len, err))
        }
        Err(task_packrat_api::EreportEncodeError::Encoder(_)) => {
            ringbuf_entry!(Trace::EreportTooBig)
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use drv_cpu_seq_api::StateChangeReason;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod gen {
    include!(concat!(env!("OUT_DIR"), "/cosmo_fpga.rs"));
}

mod fmc_periph {
    include!(concat!(env!("OUT_DIR"), "/fmc_periph.rs"));
}
use fmc_periph::sequencer as fmc_sequencer;

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
include!(concat!(env!("OUT_DIR"), "/gpio_irq_pins.rs"));
