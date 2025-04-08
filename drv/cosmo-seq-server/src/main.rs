// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Grapefruit FPGA process.

#![no_std]
#![no_main]

use drv_cpu_seq_api::{PowerState, SeqError as CpuSeqError, StateChangeReason};
use drv_ice40_spi_program as ice40;
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

task_slot!(JEFE, jefe);
task_slot!(LOADER, spartan7_loader);
task_slot!(HF, hf);
task_slot!(SYS, sys);
task_slot!(SPI_FRONT, spi_front);
task_slot!(AUXFLASH, auxflash);

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    FpgaInit,
    StartFailed(#[count(children)] SeqError),
    ContinueBitstreamLoad(usize),
    WaitForDone,
    Programmed,

    Startup {
        early_power_rdbks: fmc_periph::EarlyPowerRdbksDebug,
    },
    RegStateValues {
        seq_api_status: fmc_periph::SeqApiStatusDebug,
        seq_raw_status: fmc_periph::SeqRawStatusDebug,
        nic_api_status: fmc_periph::NicApiStatusDebug,
        nic_raw_status: fmc_periph::NicRawStatusDebug,
    },
    RegPgValues {
        rail_pgs: fmc_periph::RailPgsDebug,
        rail_pgs_max_hold: fmc_periph::RailPgsMaxHoldDebug,
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
    PowerDownError(drv_cpu_seq_api::SeqError),

    #[count(skip)]
    None,
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
const SP_CHASSIS_STATUS_LED: sys_api::PinSet = sys_api::Port::C.pin(6);

// Disabled due to hardware-cosmo#659 (on Cosmo rev A this is PB7, but we need
// to use that pin for FMC).
const SP_TO_IGN_TRGT_FPGA_FAULT_L: Option<sys_api::PinSet> = None;

////////////////////////////////////////////////////////////////////////////////

/// Helper type which includes both sequencer and NIC state machine states
struct StateMachineStates {
    seq: Result<fmc_periph::A0Sm, u8>,
    nic: Result<fmc_periph::NicSm, u8>,
}

#[export_name = "main"]
fn main() -> ! {
    match init() {
        // Set up everything nicely, time to start serving incoming messages.
        Ok(mut server) => {
            // Mark that we've reached A2, and turn on the chassis LED
            // server.sys.gpio_set(SP_CHASSIS_STATUS_LED);

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

fn init() -> Result<ServerImpl, SeqError> {
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

    let spi_front = drv_spi_api::Spi::from(SPI_FRONT.get_task_id());
    let aux = drv_auxflash_api::AuxFlash::from(AUXFLASH.get_task_id());

    // Wait for the Spartan-7 to be loaded
    let loader = Spartan7Loader::from(LOADER.get_task_id());
    let token = loader.get_token();

    init_front_fpga(
        &sys,
        &spi_front.device(drv_spi_api::devices::MUX),
        &aux,
        &ice40::Config {
            creset: sys_api::Port::A.pin(4),
            cdone: sys_api::Port::A.pin(3),
        },
    )?;

    // Bring up the SP5 NMI pin
    sys.gpio_set(SP_TO_SP5_NMI_SYNC_FLOOD_L);
    sys.gpio_configure_output(
        SP_TO_SP5_NMI_SYNC_FLOOD_L,
        sys_api::OutputType::OpenDrain,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );

    // Clear the fault pin
    if let Some(pin) = SP_TO_IGN_TRGT_FPGA_FAULT_L {
        sys.gpio_set(pin);
    }

    Ok(ServerImpl::new(token))
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
}

impl ServerImpl {
    fn new(token: drv_spartan7_loader_api::Spartan7Token) -> Self {
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

        ServerImpl {
            state: PowerState::A2,
            jefe: Jefe::from(JEFE.get_task_id()),
            sys: Sys::from(SYS.get_task_id()),
            hf: HostFlash::from(HF.get_task_id()),
            seq,
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
    ) -> Result<(), CpuSeqError> {
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
                self.seq.power_ctrl.modify(|m| m.set_a0_en(true));
                let mut okay = false;
                // Wait 2 seconds for power-up
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

                // Flip the host flash mux so the CPU can read from it
                // (this is secretly infallible on Cosmo, so we can unwrap it)
                self.hf.set_mux(drv_hf_api::HfMuxState::HostCPU).unwrap();
            }
            (PowerState::A0, PowerState::A2)
            | (PowerState::A0PlusHP, PowerState::A2)
            | (PowerState::A0Thermtrip, PowerState::A2) => {
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
            }

            // This is purely an accounting change
            (PowerState::A0, PowerState::A0PlusHP) => (),

            _ => return Err(CpuSeqError::IllegalTransition),
        }

        self.state = state;
        self.jefe.set_state(state as u32);
        self.poke_timer();
        Ok(())
    }

    /// Returns the current timer interval, in milliseconds
    ///
    /// If we are in `A0`, then we are waiting for the NIC to come up; if we are
    /// in `A0PlusHP`, we're polling for a thermtrip or for someone disabling
    /// the NIC.  In other states, there's no need to poll.
    fn poll_interval(&self) -> Option<u32> {
        match self.state {
            PowerState::A0 => Some(10),
            PowerState::A0PlusHP => Some(100),
            _ => None,
        }
    }

    /// Updates the system timer
    fn poke_timer(&self) {
        if let Some(interval) = self.poll_interval() {
            set_timer_relative(interval, notifications::TIMER_MASK);
        }
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
    ) -> Result<(), RequestError<CpuSeqError>> {
        self.set_state_impl(state, StateChangeReason::Other)?;
        Ok(())
    }

    fn set_state_with_reason(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
        reason: StateChangeReason,
    ) -> Result<(), RequestError<CpuSeqError>> {
        self.set_state_impl(state, reason)?;
        Ok(())
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
        // XXX todo
        Ok([0; 64])
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if (bits & notifications::TIMER_MASK) == 0 {
            return;
        }
        let state = self.log_state_registers();
        use fmc_periph::{A0Sm, NicSm};

        // Detect unexpected power-off
        match (self.state, state.seq) {
            (PowerState::A0 | PowerState::A0PlusHP, Ok(A0Sm::Done)) => (),
            (PowerState::A0 | PowerState::A0PlusHP, seq_state) => {
                ringbuf_entry!(Trace::UnexpectedPowerOff {
                    our_state: self.state,
                    seq_state,
                });
                self.log_pg_registers();

                // Power down to A2, updating our internal state.  We can't
                // handle errors here, so log them and continue.
                if let Err(e) = self
                    .set_state_impl(PowerState::A2, StateChangeReason::Other)
                {
                    ringbuf_entry!(Trace::PowerDownError(e))
                }
            }
            // TODO are there other states that we should check here?
            _ => (),
        }

        // Detect when the NIC comes online
        match (self.state, state.nic) {
            (PowerState::A0, Ok(NicSm::Done)) => {
                self.set_state_impl(
                    PowerState::A0PlusHP,
                    StateChangeReason::InitialPowerOn,
                )
                .unwrap(); // this should be infallible
            }
            // TODO: should we handle the NIC powering down while the main CPU
            // power remains up?
            _ => (),
        }

        self.poke_timer();
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use drv_cpu_seq_api::{SeqError, StateChangeReason};
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod gen {
    include!(concat!(env!("OUT_DIR"), "/cosmo_fpga.rs"));
}

mod fmc_periph {
    include!(concat!(env!("OUT_DIR"), "/fmc_sequencer.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
