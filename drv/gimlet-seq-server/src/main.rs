// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Gimlet sequencing process.

#![no_std]
#![no_main]

mod seq_spi;
mod vcore;

use counters::*;
use fixedstr::FixedStr;
use ringbuf::*;
use userlib::{
    hl, set_timer_relative, sys_get_timer, sys_recv_notification,
    sys_set_timer, task_slot, units, RecvMessage, TaskId, UnwrapLite,
};
use zerocopy::IntoBytes;

use drv_cpu_seq_api::{PowerState, SeqError, StateChangeReason, Transition};
use drv_hf_api as hf_api;
use drv_i2c_api as i2c;
use drv_ice40_spi_program as ice40;
use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};
use drv_spi_api::{SpiDevice, SpiServer};
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{NotificationHandler, RequestError};
use seq_spi::{Addr, Reg};
use static_assertions::const_assert;
use task_jefe_api::Jefe;

task_slot!(SYS, sys);
task_slot!(SPI, spi_driver);
task_slot!(I2C, i2c_driver);
task_slot!(HF, hf);
task_slot!(JEFE, jefe);
task_slot!(PACKRAT, packrat);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[cfg_attr(target_board = "gimlet-b", path = "payload_b.rs")]
#[cfg_attr(
    any(
        target_board = "gimlet-c",
        target_board = "gimlet-d",
        target_board = "gimlet-e",
        target_board = "gimlet-f",
    ),
    path = "payload_cdef.rs"
)]
mod payload;

/// Types for more ergonomic access to FPGA generated types
pub type A1SmStatus = Reg::A1SMSTATUS::A1SmEncoded;
pub type A0SmStatus = Reg::A0SMSTATUS::A0SmEncoded;

#[derive(Copy, Clone, PartialEq, Count)]
enum I2cTxn {
    SpdLoad(u8, u8),
    SpdLoadTop(u8, u8),
    VCoreOn,
    VCoreOff,
    VCoreUndervoltageInitialize,
    VCorePmbusStatus,
    SocOn,
    SocOff,
}

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    #[count(skip)]
    None,
    Ice40Rails(bool, bool),
    IdentValid(#[count(children)] bool),
    ChecksumValid(#[count(children)] bool),
    Reprogram(#[count(children)] bool),
    Programmed,
    Programming,
    Ice40PowerGoodV1P2(#[count(children)] bool),
    Ice40PowerGoodV3P3(#[count(children)] bool),
    RailsOff,
    Ident(u16),
    A2Status(u8),
    A2,
    A0FailureDetails(Addr, u8),
    A0Failed(#[count(children)] SeqError),
    A1Status(Result<A1SmStatus, u8>),
    A1Readbacks(u8),
    A1OutStatus(u8),
    CPUPresent(#[count(children)] bool),
    Coretype {
        coretype: bool,
        sp3r1: bool,
        sp3r2: bool,
    },
    A0Status(Result<A0SmStatus, u8>),
    A0Power(u8),
    NICPowerEnableLow(bool),
    RailsOn,
    UartEnabled,
    A0(u16),
    SetState {
        prev: PowerState,
        next: PowerState,
        #[count(children)]
        why: StateChangeReason,
        now: u64,
    },
    UpdateState(#[count(children)] PowerState),
    ClockConfigWrite,
    ClockConfigSuccess,
    Status {
        ier: u8,
        ifr: u8,
        amd_status: u8,
        amd_a0: u8,
    },
    PGStatus {
        b_pg: u8,
        c_pg: u8,
        nic: u8,
    },
    SMStatus {
        a1: Result<A1SmStatus, u8>,
        a0: Result<A0SmStatus, u8>,
    },
    NICStatus {
        nic_ctrl: u8,
        nic_status: u8,
        out_status_nic1: u8,
        out_status_nic2: u8,
    },
    ResetCounts {
        rstn: u8,
        pwrokn: u8,
    },
    PowerControl(u8),
    InterruptFlags(u8),
    V3P3SysA0VOut(units::Volts),

    SpdBankAbsent(u8),
    SpdAbsent(u8, u8, u8),
    SpdDimmsFound(usize),
    I2cError {
        txn: I2cTxn,
        #[count(children)]
        code: i2c::ResponseCode,
    },
    I2cFault(I2cTxn),
    I2cRetry {
        #[count(children)]
        txn: I2cTxn,
        retries_remaining: u8,
    },
    StartFailed(#[count(children)] SeqError),
}

counted_ringbuf!(Trace, 128, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let jefe = Jefe::from(JEFE.get_task_id());
    let spi = drv_spi_api::Spi::from(SPI.get_task_id());
    let hf = hf_api::HostFlash::from(HF.get_task_id());
    match ServerImpl::init(&sys, jefe, spi, hf) {
        // Set up everything nicely, time to start serving incoming messages.
        Ok(mut server) => {
            let mut buffer = [0; idl::INCOMING_SIZE];
            loop {
                idol_runtime::dispatch(&mut buffer, &mut server);
            }
        }

        // Initializing the sequencer failed.
        Err(_) => {
            // Tell everyone that something's broken, as loudly as possible.
            ringbuf_entry!(Trace::StartFailed(SeqError::I2cFault));
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

struct ServerImpl<S: SpiServer> {
    state: PowerState,
    sys: sys_api::Sys,
    seq: seq_spi::SequencerFpga<S>,
    jefe: Jefe,
    hf: hf_api::HostFlash,
    vcore: vcore::VCore,
    deadline: u64,
    // Buffer for encoding ereports. This is a static so that it's not on the
    // stack when handling interrupts.
    ereport_buf: &'static mut [u8; EREPORT_BUF_LEN],
}

const TIMER_INTERVAL: u32 = 10;
const EREPORT_BUF_LEN: usize = microcbor::max_cbor_len_for!(
    task_packrat_api::Ereport<EreportClass, EreportKind>
);

#[derive(microcbor::Encode)]
pub enum EreportClass {
    #[cbor(rename = "hw.pwr.pmbus.alert")]
    PmbusAlert,
}

#[derive(microcbor::EncodeFields)]
pub(crate) enum EreportKind {
    PmbusAlert {
        refdes: FixedStr<{ crate::i2c_config::MAX_COMPONENT_ID_LEN }>,
        // 9 is the maximum length rail name used in this module (`VDD_VCORE`)
        rail: &'static FixedStr<9>,
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

impl<S: SpiServer + Clone> ServerImpl<S> {
    fn init(
        sys: &sys_api::Sys,
        jefe: Jefe,
        spi: S,
        hf: hf_api::HostFlash,
    ) -> Result<Self, i2c::ResponseCode> {
        // Ensure the SP fault pin is configured as an open-drain output, and pull
        // it low to make the sequencer restart externally visible.
        sys.gpio_configure_output(
            FAULT_PIN_L,
            sys_api::OutputType::OpenDrain,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );
        sys.gpio_reset(FAULT_PIN_L);

        // Turn off the chassis LED, in case this is a task restart (and not a
        // full chip restart, which would leave the GPIO unconfigured).
        sys.gpio_configure_output(
            CHASSIS_LED,
            sys_api::OutputType::PushPull,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );
        sys.gpio_reset(CHASSIS_LED);

        // To allow for the possibility that we are restarting, rather than
        // starting, we take care during early sequencing to _not turn anything
        // off,_ only on. This means if it was _already_ on, the outputs should not
        // glitch.

        // Unconditionally set our power-good detects as inputs.
        //
        // This is the expected reset state, but, good to be sure.
        sys.gpio_configure_input(PGS_PINS, PGS_PULL);

        // Set SP3_TO_SP_NIC_PWREN_L to be an input
        sys.gpio_configure_input(NIC_PWREN_L_PINS, NIC_PWREN_L_PULL);

        // Set all of the presence-related pins to be inputs
        sys.gpio_configure_input(CORETYPE, CORETYPE_PULL);
        sys.gpio_configure_input(CPU_PRESENT_L, CPU_PRESENT_L_PULL);
        sys.gpio_configure_input(SP3R1, SP3R1_PULL);
        sys.gpio_configure_input(SP3R2, SP3R2_PULL);

        // Unconditionally set our sequencing-related GPIOs to outputs.
        //
        // If the processor has reset, these will start out low. Since neither rail
        // has external pullups, this puts the regulators into a well-defined "off"
        // state instead of leaving them floating, which is the state when A2 power
        // starts coming up.
        //
        // If it's just our driver that has reset, this will have no effect, and
        // will continue driving the lines at whatever level we left them in.
        sys.gpio_configure_output(
            ENABLES,
            sys_api::OutputType::PushPull,
            sys_api::Speed::High,
            sys_api::Pull::None,
        );

        // To talk to the sequencer we need to configure its pins, obvs. Note that
        // the SPI and CS lines are separately managed by the SPI server; the ice40
        // crate handles the CRESETB and CDONE signals, and takes care not to
        // generate surprise resets.
        ice40::configure_pins(sys, &ICE40_CONFIG);

        let pg = sys.gpio_read_input(PGS_PORT);
        let v1p2 = pg & PG_V1P2_MASK != 0;
        let v3p3 = pg & PG_V3P3_MASK != 0;

        ringbuf_entry!(Trace::Ice40Rails(v1p2, v3p3));

        // Force iCE40 CRESETB low before turning power on. This is nice because it
        // prevents the iCE40 from racing us and deciding it should try to load from
        // Flash. TODO: this may cause trouble with hot restarts, test.
        sys.gpio_reset(ICE40_CONFIG.creset);

        // Begin, or resume, the power supply sequencing process for the FPGA. We're
        // going to be reading back our enable line states to get the real state
        // being seen by the regulators, etc.

        // The V1P2 regulator comes up first. It may already be on from a past life
        // of ours. Ensuring that it's on by writing the pin is just as cheap as
        // sensing its current state, and less code than _conditionally_ writing the
        // pin, so:
        sys.gpio_set(ENABLE_V1P2);

        // We don't actually know how long ago the regulator turned on. Could have
        // been _just now_ (above) or may have already been on. We'll use the PG pin
        // to detect when it's stable. But -- the PG pin on the LT3072 is initially
        // high when you turn the regulator on, and then takes time to drop if
        // there's a problem. So, to ensure that there has been at least 1ms since
        // regulator-on, we will delay for 2.
        hl::sleep_for(2);

        // Now, monitor the PG pin.
        loop {
            // active high
            let pg = sys.gpio_read_input(PGS_PORT) & PG_V1P2_MASK != 0;
            ringbuf_entry!(Trace::Ice40PowerGoodV1P2(pg));
            if pg {
                break;
            }

            // Do _not_ burn CPU constantly polling, it's rude. We could also set up
            // pin-change interrupts but we only do this once per power on, so it
            // seems like a lot of work.
            hl::sleep_for(2);
        }

        // We believe V1P2 is good. Now, for V3P3! Set it active (high).
        sys.gpio_set(ENABLE_V3P3);

        // Delay to be sure.
        hl::sleep_for(2);

        // Now, monitor the PG pin.
        loop {
            // active high
            let pg = sys.gpio_read_input(PGS_PORT) & PG_V3P3_MASK != 0;
            ringbuf_entry!(Trace::Ice40PowerGoodV3P3(pg));
            if pg {
                break;
            }

            // Do _not_ burn CPU constantly polling, it's rude.
            hl::sleep_for(2);
        }

        // Now, V2P5 is chained off V3P3 and comes up on its own with no
        // synchronization. It takes about 500us in practice. We'll delay for 1ms,
        // plus give the iCE40 a good 10ms to come out of power-down.
        hl::sleep_for(1 + 10);

        // Sequencer FPGA power supply sequencing (meta-sequencing?) is complete.

        // Now, let's find out if we need to program the sequencer.
        if let Some(pin) = GLOBAL_RESET {
            // Also configure our design reset net -- the signal that resets the
            // logic _inside_ the FPGA instead of the FPGA itself. We're assuming
            // push-pull because all our boards with reset nets are lacking pullups
            // right now. It's active low, so, set up the pin before exposing the
            // output to ensure we don't glitch.
            sys.gpio_set(pin);
            sys.gpio_configure_output(
                pin,
                sys_api::OutputType::PushPull,
                sys_api::Speed::High,
                sys_api::Pull::None,
            );
        }

        // If the sequencer is already loaded and operational, the design loaded
        // into it should be willing to talk to us over SPI, and should be able to
        // serve up a recognizable ident code.
        let seq = seq_spi::SequencerFpga::new(
            spi.device(drv_spi_api::devices::SEQUENCER),
        );

        // If the image announces the correct identifier and has a matching
        // bitstream checksum, then we can skip reprogramming;
        let ident_valid = seq.valid_ident();
        ringbuf_entry!(Trace::IdentValid(ident_valid));

        let checksum_valid = seq.valid_checksum();
        ringbuf_entry!(Trace::ChecksumValid(checksum_valid));

        let reprogram = !ident_valid || !checksum_valid;
        ringbuf_entry!(Trace::Reprogram(reprogram));

        // We only want to reset and reprogram the FPGA when absolutely required.
        if reprogram {
            if let Some(pin) = GLOBAL_RESET {
                // Assert the design reset signal (not the same as the FPGA
                // programming logic reset signal). We do this during reprogramming
                // to avoid weird races that make our brains hurt.
                sys.gpio_reset(pin);
            }

            // Reprogramming will continue until morale improves -- to a point.
            loop {
                let prog = spi.device(drv_spi_api::devices::ICE40);
                ringbuf_entry!(Trace::Programming);
                match reprogram_fpga(&prog, sys, &ICE40_CONFIG) {
                    Ok(()) => {
                        // yay
                        break;
                    }
                    Err(_) => {
                        // Try and put state back to something reasonable.  We
                        // don't know if we're still locked, so ignore the
                        // complaint if we're not.
                        let _ = prog.release();
                    }
                }
            }

            if let Some(pin) = GLOBAL_RESET {
                // Deassert design reset signal. We set the pin, as it's
                // active low.
                sys.gpio_set(pin);
            }

            // Store our bitstream checksum in the FPGA's checksum registers
            // (which are initialized to zero).  This value is read back before
            // programming the FPGA image (e.g. if this task restarts or the SP
            // itself is reflashed), and used to decide whether FPGA programming
            // is required.
            seq.write_checksum().unwrap_lite();
        }

        ringbuf_entry!(Trace::Programmed);

        vcore_soc_off()?;

        ringbuf_entry!(Trace::RailsOff);

        let ident = seq.read_ident().unwrap_lite();
        ringbuf_entry!(Trace::Ident(ident));

        loop {
            let mut status = [0u8];

            seq.read_bytes(Addr::PWR_CTRL, &mut status).unwrap_lite();
            ringbuf_entry!(Trace::A2Status(status[0]));

            if status[0] == 0 {
                break;
            }

            hl::sleep_for(1);
        }

        //
        // If our clock generator is configured to load from external EEPROM,
        // we need to wait for up to 150 ms here (!).
        //
        hl::sleep_for(150);

        //
        // And now load our clock configuration
        //
        let clockgen = i2c_config::devices::idt8a34003(I2C.get_task_id())[0];

        payload::idt8a3xxxx_payload(|buf| match clockgen.write(buf) {
            Err(err) => Err(err),
            Ok(_) => {
                ringbuf_entry!(Trace::ClockConfigWrite);
                Ok(())
            }
        })?;

        // Populate packrat with our mac address and identity.
        let packrat = Packrat::from(PACKRAT.get_task_id());
        read_vpd_and_load_packrat(&packrat, I2C.get_task_id());

        jefe.set_state(PowerState::A2 as u32);

        ringbuf_entry!(Trace::ClockConfigSuccess);
        ringbuf_entry_v3p3_sys_a0_vout();
        ringbuf_entry!(Trace::A2);

        // After declaring A2 but before transitioning to A0 (either automatically
        // or in response to an IPC), populate packrat with EEPROM contents for use
        // by the SPD task.
        //
        // Per JEDEC 1791.12a, we must wait for tINIT (10ms) between power on and
        // sending the first SPD command.
        hl::sleep_for(10);
        read_spd_data_and_load_packrat(&packrat, I2C.get_task_id())?;

        // Turn on the chassis LED once we reach A2
        sys.gpio_set(CHASSIS_LED);

        let (device, rail) = i2c_config::pmbus::vdd_vcore(I2C.get_task_id());

        let ereport_buf = {
            use static_cell::ClaimOnceCell;
            static EREPORT_BUF: ClaimOnceCell<[u8; EREPORT_BUF_LEN]> =
                ClaimOnceCell::new([0; EREPORT_BUF_LEN]);
            EREPORT_BUF.claim()
        };

        let mut server = Self {
            state: PowerState::A2,
            sys: sys.clone(),
            seq,
            jefe,
            hf,
            deadline: 0,
            vcore: vcore::VCore::new(sys, packrat, &device, rail),
            ereport_buf,
        };

        // Power on, unless suppressed by the `stay-in-a2` feature
        if !cfg!(feature = "stay-in-a2") {
            _ = server.set_state_internal(
                PowerState::A0,
                StateChangeReason::InitialPowerOn,
            );
        }

        //
        // Configure the NMI pin. Note that this needs to be configured as open
        // drain rather than push/pull:  SP_TO_SP3_NMI_SYNC_FLOOD_L is pulled up
        // to V3P3_SYS_A0 (by R5583) and we do not want to backdrive it when in
        // A2, lest we prevent the PCA9535 GPIO expander (U307) from resetting!
        //
        sys.gpio_set(SP_TO_SP3_NMI_SYNC_FLOOD_L);
        sys.gpio_configure_output(
            SP_TO_SP3_NMI_SYNC_FLOOD_L,
            sys_api::OutputType::OpenDrain,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );

        // Clear the external fault now that we're about to start serving messages
        // and fewer things can go wrong.
        sys.gpio_set(FAULT_PIN_L);

        Ok(server)
    }
}

impl<S: SpiServer> NotificationHandler for ServerImpl<S> {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK | self.vcore.mask()
    }

    fn handle_notification(&mut self, bits: userlib::NotificationBits) {
        if bits.check_notification_mask(self.vcore.mask()) {
            self.vcore.handle_notification(self.ereport_buf);
        }

        if !bits.has_timer_fired(notifications::TIMER_MASK) {
            return;
        }

        let ifr = self.seq.read_byte(Addr::IFR).unwrap_lite();
        ringbuf_entry!(Trace::Status {
            ier: self.seq.read_byte(Addr::IER).unwrap_lite(),
            ifr,
            amd_status: self.seq.read_byte(Addr::AMD_STATUS).unwrap_lite(),
            amd_a0: self.seq.read_byte(Addr::AMD_A0).unwrap_lite(),
        });

        if self.state == PowerState::A0 || self.state == PowerState::A0PlusHP {
            //
            // The first order of business is to check if sequencer saw a
            // falling edge on PWROK (denoting a reset) or a THERMTRIP.  If it
            // did, we will go to A0Reset or A0Thermtrip as appropriate (and
            // if both are indicated, we will clear both conditions -- but
            // land in A0Thermtrip).
            //
            self.check_reset(ifr);
            self.check_thermtrip(ifr);

            //
            // Now we need to check NIC_PWREN_L to assure that our power state
            // matches it, clearing or setting NIC_CTRL in the sequencer as
            // needed.
            //
            let sys = sys_api::Sys::from(SYS.get_task_id());
            let pwren_l = sys.gpio_read(NIC_PWREN_L_PINS) != 0;

            let cld_rst = Reg::NIC_CTRL::CLD_RST;

            match (self.state, pwren_l) {
                (PowerState::A0, false) => {
                    ringbuf_entry!(Trace::NICPowerEnableLow(pwren_l));
                    self.seq
                        .clear_bytes(Addr::NIC_CTRL, &[cld_rst])
                        .unwrap_lite();
                    self.update_state_internal(PowerState::A0PlusHP);
                }

                (PowerState::A0PlusHP, true) => {
                    ringbuf_entry!(Trace::NICPowerEnableLow(pwren_l));
                    //
                    // The NIC was powered on, but is now being powered off.
                    // Something might be wrong, so record the sequencer's NIC
                    // registers.
                    //
                    ringbuf_entry!(Trace::NICStatus {
                        nic_ctrl: self
                            .seq
                            .read_byte(Addr::NIC_CTRL)
                            .unwrap_lite(),
                        nic_status: self
                            .seq
                            .read_byte(Addr::NIC_STATUS)
                            .unwrap_lite(),
                        out_status_nic1: self
                            .seq
                            .read_byte(Addr::OUT_STATUS_NIC1)
                            .unwrap_lite(),
                        out_status_nic2: self
                            .seq
                            .read_byte(Addr::OUT_STATUS_NIC2)
                            .unwrap_lite(),
                    });

                    self.seq
                        .set_bytes(Addr::NIC_CTRL, &[cld_rst])
                        .unwrap_lite();
                    self.update_state_internal(PowerState::A0);
                }

                (PowerState::A0, true) | (PowerState::A0PlusHP, false) => {
                    //
                    // Our power state matches NIC_PWREN_L -- nothing to do
                    //
                }

                (PowerState::A0Reset, _) | (PowerState::A0Thermtrip, _) => {
                    //
                    // We must have just sent ourselves here; nothing to do.
                    //
                }

                (PowerState::A2, _) | (PowerState::A2PlusFans, _) => {
                    //
                    // We can only be in this larger block if the state is A0
                    // or A0PlusHP; we must have matched one of the arms above.
                    // (We deliberately exhaustively match on power state to
                    // force any power state addition to consider this case.)
                    //
                    unreachable!();
                }
            }
        }

        if let Some(interval) = self.poll_interval() {
            self.deadline += interval;
            sys_set_timer(Some(self.deadline), notifications::TIMER_MASK);
        }
    }
}

fn retry_i2c_txn<T, E>(
    which: I2cTxn,
    mut txn: impl FnMut() -> Result<T, E>,
) -> Result<T, i2c::ResponseCode>
where
    i2c::ResponseCode: From<E>,
{
    // Chosen by fair dice roll, seems reasonable-ish?
    let mut retries_remaining = 3;
    loop {
        match txn() {
            Ok(x) => return Ok(x),
            Err(e) => {
                let code = e.into();
                ringbuf_entry!(Trace::I2cError { txn: which, code });

                if retries_remaining == 0 {
                    ringbuf_entry!(Trace::I2cFault(which));
                    return Err(code);
                }

                ringbuf_entry!(Trace::I2cRetry {
                    txn: which,
                    retries_remaining
                });

                retries_remaining -= 1;
            }
        }
    }
}

impl<S: SpiServer> ServerImpl<S> {
    fn update_state_internal(&mut self, state: PowerState) {
        ringbuf_entry!(Trace::UpdateState(state));
        self.state = state;
        self.jefe.set_state(state as u32);
    }

    fn set_state_internal(
        &mut self,
        state: PowerState,
        why: StateChangeReason,
    ) -> Result<Transition, SeqError> {
        let sys = sys_api::Sys::from(SYS.get_task_id());

        let now = sys_get_timer().now;
        ringbuf_entry!(Trace::SetState {
            prev: self.state,
            next: state,
            why,
            now
        });

        ringbuf_entry_v3p3_sys_a0_vout();

        ringbuf_entry!(Trace::PGStatus {
            b_pg: self.seq.read_byte(Addr::GROUPB_PG).unwrap_lite(),
            c_pg: self.seq.read_byte(Addr::GROUPC_PG).unwrap_lite(),
            nic: self.seq.read_byte(Addr::NIC_STATUS).unwrap_lite(),
        });

        let a1: u8 = self.seq.read_byte(Addr::A1SMSTATUS).unwrap_lite();
        let a0: u8 = self.seq.read_byte(Addr::A0SMSTATUS).unwrap_lite();
        ringbuf_entry!(Trace::SMStatus {
            a1: A1SmStatus::try_from(a1),
            a0: A0SmStatus::try_from(a0),
        });

        ringbuf_entry!(Trace::PowerControl(
            self.seq.read_byte(Addr::PWR_CTRL).unwrap_lite(),
        ));

        ringbuf_entry!(Trace::InterruptFlags(
            self.seq.read_byte(Addr::IFR).unwrap_lite(),
        ));

        match (self.state, state) {
            (PowerState::A2, PowerState::A0) => {
                //
                // First, set our mux state to be the HostCPU
                //
                if self.hf.set_mux(hf_api::HfMuxState::HostCPU).is_err() {
                    return Err(SeqError::MuxToHostCPUFailed);
                }

                //
                // If we fail to initialize our UV warning despite retries, we
                // will drive on: the failures will be logged, and this isn't
                // strictly required to sequence.
                //
                _ = retry_i2c_txn(I2cTxn::VCoreUndervoltageInitialize, || {
                    self.vcore.initialize_uv_warning()
                });

                let start = sys_get_timer().now;
                let deadline = start + A0_TIMEOUT_MILLIS;

                //
                // We are going to pass through A1 on the way to A0.  A1 is
                // more or less an implementation detail of our journey to A0,
                // but we'll stop there long enough to check our presence and
                // CPU type:  if we don't have a CPU (or have the wrong type)
                // we want to fail cleanly rather than have the appearance of
                // failing to sequence.
                //
                let a1 = Reg::PWR_CTRL::A1PWREN;
                self.seq.set_bytes(Addr::PWR_CTRL, &[a1]).unwrap_lite();

                loop {
                    let mut readbacks = [0u8];

                    self.seq
                        .read_bytes(Addr::A1_READBACKS, &mut readbacks)
                        .unwrap_lite();
                    ringbuf_entry!(Trace::A1Readbacks(readbacks[0]));

                    let mut out_status = [0u8];

                    self.seq
                        .read_bytes(Addr::A1_OUT_STATUS, &mut out_status)
                        .unwrap_lite();
                    ringbuf_entry!(Trace::A1OutStatus(out_status[0]));

                    let mut status = [0u8];

                    self.seq
                        .read_bytes(Addr::A1SMSTATUS, &mut status)
                        .unwrap_lite();

                    let a1sm = A1SmStatus::try_from(status[0]);
                    ringbuf_entry!(Trace::A1Status(a1sm));

                    if a1sm == Ok(A1SmStatus::Done) {
                        break;
                    }

                    if sys_get_timer().now > deadline {
                        return Err(self.a0_failure(SeqError::A1Timeout));
                    }

                    hl::sleep_for(1);
                }

                //
                // Check for CPU presence first, as this is the more likely
                // failure.
                //
                let present = sys.gpio_read(CPU_PRESENT_L) == 0;
                ringbuf_entry!(Trace::CPUPresent(present));

                if !present {
                    return Err(self.a0_failure(SeqError::CPUNotPresent));
                }

                let coretype = sys.gpio_read(CORETYPE) != 0;
                let sp3r1 = sys.gpio_read(SP3R1) != 0;
                let sp3r2 = sys.gpio_read(SP3R2) != 0;

                ringbuf_entry!(Trace::Coretype {
                    coretype,
                    sp3r1,
                    sp3r2
                });

                //
                // Check that we have the type of CPU we expect:  we expect
                // CORETYPE to be high (not connected on Family 19h), SP3R1 to
                // be high (not connected on Type-0/Type-1/Type-2), and SP3R2
                // to be low (VSS on Type-0/Type-1/Type-2).
                //
                if !coretype || !sp3r1 || sp3r2 {
                    return Err(self.a0_failure(SeqError::UnrecognizedCPU));
                }

                //
                // Onward to A0!
                //
                let a0 = Reg::PWR_CTRL::A0A_EN;
                self.seq.set_bytes(Addr::PWR_CTRL, &[a0]).unwrap_lite();

                loop {
                    let mut status = [0u8];

                    self.seq
                        .read_bytes(Addr::A0SMSTATUS, &mut status)
                        .unwrap_lite();

                    let a0sm = A0SmStatus::try_from(status[0]);
                    ringbuf_entry!(Trace::A0Status(a0sm));

                    if a0sm == Ok(A0SmStatus::GroupcPg) {
                        break;
                    }

                    if sys_get_timer().now > deadline {
                        return Err(self.a0_failure(SeqError::A0TimeoutGroupC));
                    }

                    hl::sleep_for(1);
                }

                //
                // And power up!
                //
                if vcore_soc_on().is_err() {
                    // Uh-oh, the I2C write failed a bunch of times. Guess I'll
                    // die!
                    return Err(self.a0_failure(SeqError::I2cFault));
                }
                ringbuf_entry!(Trace::RailsOn);

                //
                // Now wait for the end of Group C.

                //
                loop {
                    let mut status = [0u8];

                    self.seq
                        .read_bytes(Addr::A0SMSTATUS, &mut status)
                        .unwrap_lite();
                    ringbuf_entry!(Trace::A0Power(status[0]));

                    if status[0] == Reg::A0SMSTATUS::A0SmEncoded::Done as u8 {
                        break;
                    }

                    if sys_get_timer().now > deadline {
                        return Err(self.a0_failure(SeqError::A0Timeout));
                    }

                    hl::sleep_for(1);
                }

                //
                // And establish our timer to check SP3_TO_SP_NIC_PWREN_L.
                //
                self.deadline = set_timer_relative(
                    TIMER_INTERVAL,
                    notifications::TIMER_MASK,
                );

                //
                // Finally, enable transmission to the SP3's UART
                //
                uart_sp_to_sp3_enable();
                ringbuf_entry!(Trace::UartEnabled);
                // Using wrapping_sub here because the timer is monotonic, so
                // we, the programmers, know that now > start. rustc, the
                // compiler, is not aware of this.
                ringbuf_entry!(Trace::A0(
                    (sys_get_timer().now.wrapping_sub(start)) as u16
                ));

                self.update_state_internal(PowerState::A0);
                Ok(Transition::Changed)
            }

            (PowerState::A0, PowerState::A2)
            | (PowerState::A0PlusHP, PowerState::A2)
            | (PowerState::A0Thermtrip, PowerState::A2)
            | (PowerState::A0Reset, PowerState::A2) => {
                //
                // Flip the UART mux back to disabled
                //
                uart_sp_to_sp3_disable();

                //
                // To assure that we always enter A0 the same way, set CLD_RST
                // in NIC_CTRL on our way back to A2.
                //
                let cld_rst = Reg::NIC_CTRL::CLD_RST;
                self.seq.set_bytes(Addr::NIC_CTRL, &[cld_rst]).unwrap_lite();

                //
                // Start FPGA down-sequence. Clearing the enables immediately
                // de-asserts PWR_GOOD to the SP3 processor which the EDS
                // says is required before taking the rails out.
                // We also need to be headed down before the rails get taken out
                // so as not to trip a MAPO fault.
                //
                let a1a0 = Reg::PWR_CTRL::A1PWREN | Reg::PWR_CTRL::A0A_EN;
                self.seq.clear_bytes(Addr::PWR_CTRL, &[a1a0]).unwrap_lite();

                //
                // FPGA de-asserts PWR_GOOD for 2 ms before yanking enables,
                // we wait for a tick here to make sure the SPI command to the
                // FPGA propagated and the FPGA has had time to act. AMD's EDS
                // doesn't give a minimum time so we'll give them 1 ms.
                //
                hl::sleep_for(1);

                if vcore_soc_off().is_err() {
                    return Err(SeqError::I2cFault);
                }

                if self.hf.set_mux(hf_api::HfMuxState::SP).is_err() {
                    return Err(SeqError::MuxToSPFailed);
                }

                self.update_state_internal(PowerState::A2);
                ringbuf_entry_v3p3_sys_a0_vout();
                ringbuf_entry!(Trace::A2);

                //
                // Our rails should be draining.  We'll take two additional
                // measurements (for a total of three) each 100 ms apart.
                //
                for _i in 0..2 {
                    hl::sleep_for(100);
                    ringbuf_entry_v3p3_sys_a0_vout();
                }

                Ok(Transition::Changed)
            }
            //
            // A0PlusHP is a substate of A0; if we are in A0PlusHP and we are
            // asked to go to A0, return `Unchanged`, because `A0PlusHP` means
            // we are already in A0.
            // Similarly, A2PlusFans "counts as" A2 for the purpose of
            // externally-requested transitions.
            //
            (PowerState::A0PlusHP, PowerState::A0)
            | (PowerState::A2PlusFans, PowerState::A2) => {
                Ok(Transition::Unchanged)
            }
            //
            // If we are already in the requested state, return `Unchanged`.
            //
            (current, requested) if current == requested => {
                Ok(Transition::Unchanged)
            }
            (_, _) => Err(SeqError::IllegalTransition),
        }
    }

    fn a0_failure(&mut self, err: SeqError) -> SeqError {
        let record_reg = |addr| {
            ringbuf_entry!(Trace::A0FailureDetails(
                addr,
                self.seq.read_byte(addr).unwrap_lite(),
            ));
        };

        //
        // We are not going to space today.  Record information in our ring
        // buffer to allow this to be debugged.
        //
        ringbuf_entry!(Trace::A0Failed(err));
        record_reg(Addr::IFR);
        record_reg(Addr::DBG_MAX_A0SMSTATUS);
        record_reg(Addr::MAX_GROUPB_PG);
        record_reg(Addr::MAX_GROUPC_PG);
        record_reg(Addr::FLT_A0_SMSTATUS);
        record_reg(Addr::FLT_GROUPB_PG);
        record_reg(Addr::FLT_GROUPC_PG);

        //
        // Now put ourselves back in A2.
        //
        let a1a0 = Reg::PWR_CTRL::A1PWREN | Reg::PWR_CTRL::A0A_EN;
        self.seq.clear_bytes(Addr::PWR_CTRL, &[a1a0]).unwrap_lite();

        hl::sleep_for(1);

        // If this I2C write fails, that's bad news, but we're already dying...
        let _ = vcore_soc_off();
        _ = self.hf.set_mux(hf_api::HfMuxState::SP);

        err
    }

    //
    // Check for a THERMTRIP, sending ourselves to A0Thermtrip if we've
    // seen it (and knowing that the FPGA has already taken care of the
    // time-critical bits to assure that we don't melt!).
    //
    fn check_thermtrip(&mut self, ifr: u8) {
        let thermtrip = Reg::IFR::THERMTRIP;

        if ifr & thermtrip != 0 {
            self.seq.clear_bytes(Addr::IFR, &[thermtrip]).unwrap_lite();
            self.update_state_internal(PowerState::A0Thermtrip);
        }
    }

    //
    // Check for a reset by looking for a latched falling edge on PWROK.
    // (Host software explicitly configures this by setting rsttocpupwrgden
    // in FCH::PM::RESETCONTROL1.)  The sequencer also latches the number of
    // such edges that it has seen -- along with the number of falling edges
    // of RESET_L.  If we have seen a host reset, we send ourselves to
    // A0Reset.
    //
    fn check_reset(&mut self, ifr: u8) {
        let pwrok_fedge = Reg::IFR::AMD_PWROK_FEDGE;

        if ifr & pwrok_fedge != 0 {
            let mut cnts = [0u8; 2];

            const_assert!(Addr::AMD_RSTN_CNTS.precedes(Addr::AMD_PWROKN_CNTS));
            self.seq
                .read_bytes(Addr::AMD_RSTN_CNTS, &mut cnts)
                .unwrap_lite();

            let (rstn, pwrokn) = (cnts[0], cnts[1]);
            ringbuf_entry!(Trace::ResetCounts { rstn, pwrokn });

            //
            // Clear the counts to denote that we wish to re-latch any
            // falling PWROK/RESET_L edge.
            //
            self.seq
                .write_bytes(Addr::AMD_RSTN_CNTS, &[0, 0])
                .unwrap_lite();
            let mask = pwrok_fedge | Reg::IFR::AMD_RSTN_FEDGE;
            self.seq.clear_bytes(Addr::IFR, &[mask]).unwrap_lite();

            self.update_state_internal(PowerState::A0Reset);
        }
    }

    //
    // Return the current timer interval, in milliseconds.  If we are in A0,
    // we are polling for NIC_PWREN_L; if we are in A0PlusHP, we are polling
    // for a thermtrip or for someone disabling NIC_PWREN_L.  If we are in
    // any other state, we don't need to poll.
    //
    fn poll_interval(&self) -> Option<u64> {
        match self.state {
            PowerState::A0 => Some(10),
            PowerState::A0PlusHP => Some(100),
            _ => None,
        }
    }
}

impl<S: SpiServer> idl::InOrderSequencerImpl for ServerImpl<S> {
    fn get_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<PowerState, RequestError<core::convert::Infallible>> {
        Ok(self.state)
    }

    fn set_state(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
    ) -> Result<Transition, RequestError<SeqError>> {
        self.set_state_internal(state, StateChangeReason::Other)
            .map_err(RequestError::from)
    }

    fn set_state_with_reason(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
        reason: StateChangeReason,
    ) -> Result<Transition, RequestError<SeqError>> {
        self.set_state_internal(state, reason)
            .map_err(RequestError::from)
    }

    fn send_hardware_nmi(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        // The required length for an NMI pulse is apparently not documented.
        //
        // Let's try 25 ms!
        self.sys.gpio_reset(SP_TO_SP3_NMI_SYNC_FLOOD_L);
        hl::sleep_for(25);
        self.sys.gpio_set(SP_TO_SP3_NMI_SYNC_FLOOD_L);
        Ok(())
    }

    fn read_fpga_regs(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 64], RequestError<core::convert::Infallible>> {
        let mut buf = [0; 64];
        const CHUNK_SIZE: usize = 8;
        static_assertions::const_assert!(
            CHUNK_SIZE <= seq_spi::MAX_SPI_CHUNK_SIZE
        );

        for i in (0..buf.len()).step_by(CHUNK_SIZE) {
            self.seq
                .read_bytes(i as u16, &mut buf[i..i + CHUNK_SIZE])
                // We asserted at compile time that the chunk size does not
                // exceed the maximum SPI chunk size, so this shouldn't ever
                // panic.
                .unwrap_lite();
        }

        Ok(buf)
    }

    fn last_post_code(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<core::convert::Infallible>> {
        Err(RequestError::Fail(
            idol_runtime::ClientError::BadMessageContents,
        ))
    }

    fn gpio_edge_count(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<core::convert::Infallible>> {
        let mut out = zerocopy::byteorder::big_endian::U32::new(0);
        self.seq
            .read_bytes(Addr::GPIO_EDGE_CNT_3, out.as_mut_bytes())
            .unwrap_lite();
        Ok(out.get())
    }

    fn gpio_cycle_count(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<core::convert::Infallible>> {
        let mut out = zerocopy::byteorder::big_endian::U32::new(0);
        self.seq
            .read_bytes(Addr::GPIO_CYCLE_CNT_3, out.as_mut_bytes())
            .unwrap_lite();
        Ok(out.get())
    }
}

fn read_spd_data_and_load_packrat(
    packrat: &Packrat,
    i2c_task: TaskId,
) -> Result<(), i2c::ResponseCode> {
    use drv_cpu_seq_api::NUM_SPD_BANKS;
    use drv_i2c_api::{Controller, I2cDevice, Mux, PortIndex, Segment};

    type SpdBank = (Controller, PortIndex, Option<(Mux, Segment)>);

    cfg_if::cfg_if! {
        if #[cfg(any(
            target_board = "gimlet-b",
            target_board = "gimlet-c",
            target_board = "gimlet-d",
            target_board = "gimlet-e",
            target_board = "gimlet-f",
        ))] {
            //
            // On Gimlet, we have two banks of up to 8 DIMMs apiece:
            //
            // - ABCD DIMMs are on the mid bus (I2C3, port H)
            // - EFGH DIMMS are on the rear bus (I2C4, port F)
            //
            // It should go without saying that the ordering here is essential
            // to assure that the SPD data that we return for a DIMM corresponds
            // to the correct DIMM from the SoC's perspective.
            //
            const BANKS: [SpdBank; NUM_SPD_BANKS] = [
                (Controller::I2C3, i2c_config::ports::i2c3_h(), None),
                (Controller::I2C4, i2c_config::ports::i2c4_f(), None),
            ];
        } else {
            compile_error!("I2C target unsupported for this board");
        }
    }

    let mut npresent = 0;
    let mut present = [false; BANKS.len() * spd::MAX_DEVICES as usize];
    let mut tmp = [0u8; 256];

    // For each bank, we're going to iterate over each device, reading all 512
    // bytes of SPD data from each.
    for nbank in 0..BANKS.len() as u8 {
        let (controller, port, mux) = BANKS[nbank as usize];

        let addr = spd::Function::PageAddress(spd::Page(0))
            .to_device_code()
            .unwrap_lite();
        let page =
            I2cDevice::new(i2c_task, controller, port, None, addr, "SPD");

        if page.write(&[0]).is_err() {
            // If our operation fails, we are going to assume that there
            // are no DIMMs on this bank.
            ringbuf_entry!(Trace::SpdBankAbsent(nbank));
            continue;
        }

        for i in 0..spd::MAX_DEVICES {
            let mem = spd::Function::Memory(i).to_device_code().unwrap_lite();
            let spd =
                I2cDevice::new(i2c_task, controller, port, mux, mem, "SPD");
            let ndx = (nbank * spd::MAX_DEVICES) + i;

            // Try reading the first byte; if this fails, we will assume
            // the device isn't present.
            let first = match spd.read_reg::<u8, u8>(0) {
                Ok(val) => {
                    present[usize::from(ndx)] = true;
                    npresent += 1;
                    val
                }
                Err(_) => {
                    ringbuf_entry!(Trace::SpdAbsent(nbank, i, ndx));
                    continue;
                }
            };

            // We'll store that byte and then read 255 more.
            tmp[0] = first;

            let mut retried = false;

            retry_i2c_txn(I2cTxn::SpdLoad(nbank, i), || {
                if retried {
                    //
                    // If our read needs to be retried, we need to also reset
                    // ourselves back to the 0th byte.
                    //
                    _ = spd.read_reg::<u8, u8>(0)?;
                }

                retried = true;
                spd.read_into(&mut tmp[1..])
            })?;

            packrat.set_spd_eeprom(ndx, 0, &tmp);
        }

        // Now flip over to the top page.
        let addr = spd::Function::PageAddress(spd::Page(1))
            .to_device_code()
            .unwrap_lite();
        let page =
            I2cDevice::new(i2c_task, controller, port, None, addr, "SPD");

        // We really don't expect this to fail, and if it does, tossing here
        // seems to be best option:  things are pretty wrong.
        page.write(&[0]).unwrap_lite();

        // ...and two more reads for each (present) device.
        for i in 0..spd::MAX_DEVICES {
            let ndx = (nbank * spd::MAX_DEVICES) + i;

            if !present[usize::from(ndx)] {
                continue;
            }

            let mem = spd::Function::Memory(i).to_device_code().unwrap_lite();
            let spd =
                I2cDevice::new(i2c_task, controller, port, mux, mem, "SPD");

            let chunk = 128;

            retry_i2c_txn(I2cTxn::SpdLoadTop(nbank, i), || {
                //
                // Both of these reads need to be in a single transaction from
                // the perspective of the retry logic: if either fails, we
                // must redo both.
                //
                spd.read_reg_into::<u8>(0, &mut tmp[..chunk])?;
                spd.read_into(&mut tmp[chunk..])
            })?;

            packrat.set_spd_eeprom(ndx, spd::PAGE_SIZE, &tmp);
        }
    }

    ringbuf_entry!(Trace::SpdDimmsFound(npresent));
    Ok(())
}

fn reprogram_fpga<S: SpiServer>(
    spi: &SpiDevice<S>,
    sys: &sys_api::Sys,
    config: &ice40::Config,
) -> Result<(), ice40::Ice40Error> {
    ice40::begin_bitstream_load(spi, sys, config)?;

    // We've got the bitstream in Flash, so we can technically just send it in
    // one transaction, but we'll want chunking later -- so let's make sure
    // chunking works.
    let mut bitstream = COMPRESSED_BITSTREAM;
    let mut decompressor = gnarle::Decompressor::default();
    let mut chunk = [0; 256];
    while !bitstream.is_empty() || !decompressor.is_idle() {
        let out =
            gnarle::decompress(&mut decompressor, &mut bitstream, &mut chunk);
        ice40::continue_bitstream_load(spi, out)?;
    }

    ice40::finish_bitstream_load(spi, sys, config)
}

static COMPRESSED_BITSTREAM: &[u8] =
    include_bytes!(env!("GIMLET_FPGA_IMAGE_PATH"));

cfg_if::cfg_if! {
    if #[cfg(any(
        target_board = "gimlet-b",
        target_board = "gimlet-c",
        target_board = "gimlet-d",
        target_board = "gimlet-e",
        target_board = "gimlet-f",
    ))] {
        const A0_TIMEOUT_MILLIS: u64 = 2000;

        const ICE40_CONFIG: ice40::Config = ice40::Config {
            // CRESET net is SEQ_TO_SP_CRESET_L and hits PD5.
            creset: sys_api::Port::D.pin(5),

            // CDONE net is SEQ_TO_SP_CDONE_L and hits PB4.
            cdone: sys_api::Port::B.pin(4),
        };

        const GLOBAL_RESET: Option<sys_api::PinSet> = Some(
            sys_api::Port::A.pin(6)
        );

        const SP_TO_SP3_NMI_SYNC_FLOOD_L: sys_api::PinSet =
            sys_api::Port::J.pin(2);

        //
        // SP_TO_SP3_UARTA_OE_L must be driven low to allow for transmission
        // into the SP3's UART
        //
        const UART_TX_ENABLE: sys_api::PinSet = sys_api::Port::A.pin(5);

        fn uart_sp_to_sp3_enable() {
            let sys = sys_api::Sys::from(SYS.get_task_id());

            sys.gpio_configure_output(
                UART_TX_ENABLE,
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
            );

            sys.gpio_reset(UART_TX_ENABLE);
        }

        fn uart_sp_to_sp3_disable() {
            let sys = sys_api::Sys::from(SYS.get_task_id());

            sys.gpio_configure_output(
                UART_TX_ENABLE,
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
            );

            sys.gpio_set(UART_TX_ENABLE);
        }
        const ENABLES_PORT: sys_api::Port = sys_api::Port::A;
        const ENABLE_V1P2_MASK: u16 = 1 << 15;
        const ENABLE_V3P3_MASK: u16 = 1 << 4;

        const ENABLES: sys_api::PinSet = sys_api::PinSet {
            port: ENABLES_PORT,
            pin_mask: ENABLE_V1P2_MASK | ENABLE_V3P3_MASK,
        };

        const ENABLE_V1P2: sys_api::PinSet = sys_api::PinSet {
            port: ENABLES_PORT,
            pin_mask: ENABLE_V1P2_MASK,
        };

        const ENABLE_V3P3: sys_api::PinSet = sys_api::PinSet {
            port: ENABLES_PORT,
            pin_mask: ENABLE_V3P3_MASK,
        };

        const PGS_PORT: sys_api::Port = sys_api::Port::C;
        const PG_V1P2_MASK: u16 = 1 << 7;
        const PG_V3P3_MASK: u16 = 1 << 6;

        const PGS_PINS: sys_api::PinSet = sys_api::PinSet {
            port: PGS_PORT,
            pin_mask: PG_V1P2_MASK | PG_V3P3_MASK
        };

        // SP_STATUS_LED
        const CHASSIS_LED: sys_api::PinSet = sys_api::Port::A.pin(3);
        // SP_TO_IGNIT_FAULT_L
        const FAULT_PIN_L: sys_api::PinSet = sys_api::Port::A.pin(15);

        // Gimlet provides external pullups.
        const PGS_PULL: sys_api::Pull = sys_api::Pull::None;

        const NIC_PWREN_L_PINS: sys_api::PinSet = sys_api::Port::F.pin(4);

        // Externally pulled to V3P3_SYS_A0
        const NIC_PWREN_L_PULL: sys_api::Pull = sys_api::Pull::None;

        // Pins related to core type and presence
        const CORETYPE: sys_api::PinSet = sys_api::Port::I.pin(5);
        const CPU_PRESENT_L: sys_api::PinSet = sys_api::Port::C.pin(13);
        const SP3R1: sys_api::PinSet = sys_api::Port::I.pin(4);
        const SP3R2: sys_api::PinSet = sys_api::Port::H.pin(13);

        // All of these are externally pulled to V3P3_SP3_VDD_33_S5_A1
        const CORETYPE_PULL: sys_api::Pull = sys_api::Pull::None;
        const CPU_PRESENT_L_PULL: sys_api::Pull = sys_api::Pull::None;
        const SP3R1_PULL: sys_api::Pull = sys_api::Pull::None;
        const SP3R2_PULL: sys_api::Pull = sys_api::Pull::None;

        fn vcore_soc_off() -> Result<(), i2c::ResponseCode> {
            use drv_i2c_devices::raa229618::Raa229618;
            let i2c = I2C.get_task_id();

            let (device, rail) = i2c_config::pmbus::vdd_vcore(i2c);
            let mut vdd_vcore = Raa229618::new(&device, rail);

            let (device, rail) = i2c_config::pmbus::vddcr_soc(i2c);
            let mut vddcr_soc = Raa229618::new(&device, rail);

            retry_i2c_txn(I2cTxn::VCoreOff, || vdd_vcore.turn_off())?;
            retry_i2c_txn(I2cTxn::SocOff, || vddcr_soc.turn_off())?;
            Ok(())
        }

        fn vcore_soc_on() -> Result<(), i2c::ResponseCode> {
            use drv_i2c_devices::raa229618::Raa229618;
            let i2c = I2C.get_task_id();

            let (device, rail) = i2c_config::pmbus::vdd_vcore(i2c);
            let mut vdd_vcore = Raa229618::new(&device, rail);

            let (device, rail) = i2c_config::pmbus::vddcr_soc(i2c);
            let mut vddcr_soc = Raa229618::new(&device, rail);

            retry_i2c_txn(I2cTxn::VCoreOn, || vdd_vcore.turn_on())?;
            retry_i2c_txn(I2cTxn::SocOn, || vddcr_soc.turn_on())?;
            Ok(())
        }

        //
        // We have had issues whereby V3P3_SYS_A0 is inadvertently driven by a
        // pin on the SP (e.g., a pin that is pulled up to V3P3_SYS_A0 being
        // configured as a push/pull GPIO rather than open drain).  The side
        // effects of this are nasty from the SP3 side, so to help debug any
        // such inadvertent driving, we record the Vout of V3P3_SYS_A0 after
        // arriving in A2 and then before starting any state transition; this
        // convenience routine makes these recordings easy to sprinkle as
        // needed.
        //
        fn ringbuf_entry_v3p3_sys_a0_vout() {
            use drv_i2c_devices::tps546b24a::Tps546B24A;
            use drv_i2c_devices::VoltageSensor;

            let i2c = I2C.get_task_id();

            let (device, rail) = i2c_config::pmbus::v3p3_sys_a0(i2c);
            let v3p3_sys_a0 = Tps546B24A::new(&device, rail);

            ringbuf_entry!(
                Trace::V3P3SysA0VOut(v3p3_sys_a0.read_vout().unwrap_lite())
            );
        }
    } else {
        compile_error!("unsupported target board");
    }
}

mod idl {
    use super::StateChangeReason;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
include!(concat!(env!("OUT_DIR"), "/gpio_irq_pins.rs"));
