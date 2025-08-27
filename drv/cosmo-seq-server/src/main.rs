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
        early_power_rdbks: fmc_periph::EarlyPowerRdbksView,
    },
    RegStateValues {
        seq_api_status: fmc_periph::SeqApiStatusView,
        seq_raw_status: fmc_periph::SeqRawStatusView,
        nic_api_status: fmc_periph::NicApiStatusView,
        nic_raw_status: fmc_periph::NicRawStatusView,
    },
    RegPgValues {
        rail_pgs: fmc_periph::RailPgsView,
        rail_pgs_max_hold: fmc_periph::RailPgsMaxHoldView,
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
        seq_state: Result<fmc_periph::A0Sm, u8>,
    },
    SequencerInterrupt {
        our_state: PowerState,
        seq_state: Result<fmc_periph::A0Sm, u8>,
        ifr: fmc_periph::IfrView,
    },
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
    SmerrInterrupt,
    PmbusAlert {
        now: u64,
    },
    UnexpectedInterrupt,
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
    seq: Result<fmc_periph::A0Sm, u8>,
    nic: Result<fmc_periph::NicSm, u8>,
}

#[export_name = "main"]
fn main() -> ! {
    // Populate packrat with our mac address and identity.
    let packrat = Packrat::from(PACKRAT.get_task_id());
    read_vpd_and_load_packrat(&packrat, I2C.get_task_id());

    match init(packrat) {
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

fn init(packrat: Packrat) -> Result<ServerImpl, SeqError> {
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
    let info = fmc_periph::Info::new(token);
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

    let token = loader.get_token();
    Ok(ServerImpl::new(token, packrat))
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
    seq: fmc_periph::Sequencer,
    vcore: VCore,
}

impl ServerImpl {
    fn new(
        token: drv_spartan7_loader_api::Spartan7Token,
        packrat: Packrat,
    ) -> Self {
        let now = sys_get_timer().now;
        let seq = fmc_periph::Sequencer::new(token);
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
            vcore: VCore::new(I2C.get_task_id(), packrat),
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

        use fmc_periph::A0Sm;
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
                            // We have an outstanding issue on v1 hardware-cosmo#658
                            // that prevents us from checking `CPU_PRESENT` at
                            // `A0Sm::ENABLE_GRP_A` time
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

                    // XXX faulted isn't strictly a timeout, but this is the
                    // closest available error code
                    return Err(CpuSeqError::A0Timeout);
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
            m.set_fanfault(false);
            m.set_thermtrip(false);
            m.set_smerr_assert(false);
            m.set_a0mapo(false);
            m.set_nicmapo(false);
            m.set_amd_pwrok_fedge(false);
            m.set_amd_rstn_fedge(false);
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
        // few times, so we should power through any transient I2C messiness.
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

        let state = self.log_state_registers();
        ringbuf_entry!(Trace::SequencerInterrupt {
            our_state: self.state,
            seq_state: state.seq,
            ifr,
        });

        enum InternalAction {
            Reset,
            ThermTrip,
            Smerr,
            Mapo,
            None,
            Unexpected,
        }

        // We check these in lowest to highest priority. We start with
        // reset since we expect the CPU to handle that nicely.
        // Thermal trip is a terminal state in that we log it but don't
        // actually make any changes to the sequencer.
        // SMERR is treated as a higher priority than MAPO arbitrarily.
        // we probably(?) won't see multiple of these set at a time but
        // it's important to account for that case;

        let mut action = InternalAction::Unexpected;

        if ifr.pwr_cont1_to_fpga1_alert || ifr.pwr_cont2_to_fpga1_alert {
            // We got a PMBus alert from one of the Vcore regulators.
            let now = sys_get_timer().now;
            ringbuf_entry!(Trace::PmbusAlert { now });
            let which_rails = vcore::Rails {
                vddcr_cpu0: ifr.pwr_cont1_to_fpga1_alert,
                vddcr_cpu1: ifr.pwr_cont2_to_fpga1_alert,
            };
            self.vcore.handle_pmalert(which_rails, now);

            // If *all* we saw was a PMBus alert, don't reset --- perhaps we're
            // still fine, and we just got a warning from the regulator. If
            // POWER_GOOD was deasserted, then the FPGA will MAPO us anyway,
            // even though clearing the fault in the regulator might make
            // POWER_GOOD come back.
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

        if ifr.thermtrip {
            self.seq.ifr.modify(|h| h.set_thermtrip(false));
            ringbuf_entry!(Trace::Thermtrip);
            action = InternalAction::ThermTrip;
            // Great place for an ereport?
        }

        if ifr.a0mapo {
            self.log_pg_registers();
            self.seq.ifr.modify(|h| h.set_a0mapo(false));
            ringbuf_entry!(Trace::A0MapoInterrupt);
            action = InternalAction::Mapo;
            // Great place for an ereport?
        }

        if ifr.smerr_assert {
            self.seq.ifr.modify(|h| h.set_smerr_assert(false));
            ringbuf_entry!(Trace::SmerrInterrupt);
            action = InternalAction::Smerr;
            // Great place for an ereport?
        }
        // Fan Fault is unconnected
        // NIC MAPO is unconnected

        match action {
            InternalAction::Reset => {
                // host_sp_comms will be notified of this change and will
                // call back into this task to reboot the system (going to
                // A2 then back into A0)
                self.set_state_internal(PowerState::A0Reset);
            }
            InternalAction::ThermTrip => {
                // This is a terminal state; we set our state to `A0Thermtrip`
                // but do not expect any other task to take action right now
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
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK | notifications::SEQ_IRQ_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if (bits & notifications::SEQ_IRQ_MASK) != 0 {
            self.handle_sequencer_interrupt();
        }

        if (bits & notifications::TIMER_MASK) == 0 {
            return;
        }
        let state = self.log_state_registers();
        use fmc_periph::{A0Sm, NicSm};

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

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use drv_cpu_seq_api::StateChangeReason;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod gen {
    include!(concat!(env!("OUT_DIR"), "/cosmo_fpga.rs"));
}

mod fmc_periph {
    include!(concat!(env!("OUT_DIR"), "/fmc_sequencer.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
include!(concat!(env!("OUT_DIR"), "/gpio_irq_pins.rs"));
