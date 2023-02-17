// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Gimlet sequencing process.

#![no_std]
#![no_main]

mod seq_spi;

use ringbuf::*;
use userlib::*;

use drv_gimlet_hf_api as hf_api;
use drv_gimlet_seq_api::{PowerState, SeqError};
use drv_ice40_spi_program as ice40;
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

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[cfg_attr(target_board = "gimlet-b", path = "payload_b.rs")]
#[cfg_attr(target_board = "gimlet-c", path = "payload_c.rs")]
mod payload;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Ice40Rails(bool, bool),
    IdentValid(bool),
    ChecksumValid(bool),
    Reprogram(bool),
    Programmed,
    Programming,
    Ice40PowerGoodV1P2(bool),
    Ice40PowerGoodV3P3(bool),
    RailsOff,
    Ident(u16),
    A1Status(u8),
    A2,
    A1Power(u8, u8),
    A0Power(u8),
    NICPowerEnableLow(bool),
    RailsOn,
    UartEnabled,
    SetState(PowerState, PowerState),
    UpdateState(PowerState),
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
        a1: u8,
        a0: u8,
    },
    ResetCounts {
        rstn: u8,
        pwrokn: u8,
    },
    RailStatusCore(u16),
    RailErrorCore(drv_i2c_devices::raa229618::Error),
    RailStatusSoc(u16),
    RailErrorSoc(drv_i2c_devices::raa229618::Error),
    PowerControl(u8),
    InterruptFlags(u8),
    None,
}

ringbuf!(Trace, 100, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let spi = drv_spi_api::Spi::from(SPI.get_task_id());
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let jefe = Jefe::from(JEFE.get_task_id());
    let hf = hf_api::HostFlash::from(HF.get_task_id());

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
    ice40::configure_pins(&sys, &ICE40_CONFIG);

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

    if let Some(hacks) = FPGA_HACK_PINS {
        // Some boards require certain pins to be put in certain states before
        // we can perform SPI communication with the design (rather than the
        // programming port). If this is such a board, apply those changes:
        for &(pin, is_high) in hacks {
            if is_high {
                sys.gpio_set(pin);
            } else {
                sys.gpio_reset(pin);
            }

            sys.gpio_configure_output(
                pin,
                sys_api::OutputType::PushPull,
                sys_api::Speed::High,
                sys_api::Pull::None,
            );
        }
    }

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
            match reprogram_fpga(&prog, &sys, &ICE40_CONFIG) {
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
        seq.write_checksum().unwrap();
    }

    ringbuf_entry!(Trace::Programmed);

    vcore_soc_off();
    ringbuf_entry!(Trace::RailsOff);

    let ident = seq.read_ident().unwrap();
    ringbuf_entry!(Trace::Ident(ident));

    loop {
        let mut status = [0u8];

        seq.read_bytes(Addr::PWR_CTRL, &mut status).unwrap();
        ringbuf_entry!(Trace::A1Status(status[0]));

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
    })
    .unwrap();

    jefe.set_state(PowerState::A2 as u32);

    ringbuf_entry!(Trace::ClockConfigSuccess);
    ringbuf_entry!(Trace::A2);

    // Turn on the chassis LED once we reach A2
    sys.gpio_set(CHASSIS_LED);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        state: PowerState::A2,
        sys: sys.clone(),
        seq,
        jefe,
        hf,
        deadline: 0,
    };

    // Power on, unless suppressed by the `stay-in-a2` feature
    if !cfg!(feature = "stay-in-a2") {
        server.set_state_internal(PowerState::A0).unwrap();
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

    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

struct ServerImpl<S: SpiServer> {
    state: PowerState,
    sys: sys_api::Sys,
    seq: seq_spi::SequencerFpga<S>,
    jefe: Jefe,
    hf: hf_api::HostFlash,
    deadline: u64,
}

const TIMER_INTERVAL: u64 = 10;

impl<S: SpiServer> NotificationHandler for ServerImpl<S> {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        ringbuf_entry!(Trace::Status {
            ier: self.seq.read_byte(Addr::IER).unwrap(),
            ifr: self.seq.read_byte(Addr::IFR).unwrap(),
            amd_status: self.seq.read_byte(Addr::AMD_STATUS).unwrap(),
            amd_a0: self.seq.read_byte(Addr::AMD_A0).unwrap(),
        });

        if self.state == PowerState::A0 || self.state == PowerState::A0PlusHP {
            //
            // The first order of business is to check if sequencer saw a
            // falling edge on PWROK (denoting a reset) or a THERMTRIP.  If it
            // did, we will go to A0Reset or A0Thermtrip as appropriate (and
            // if both are indicated, we will clear both conditions -- but
            // land in A0Thermtrip).
            //
            let ifr = self.seq.read_byte(Addr::IFR).unwrap();
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
                    self.seq.clear_bytes(Addr::NIC_CTRL, &[cld_rst]).unwrap();
                    self.update_state_internal(PowerState::A0PlusHP);
                }

                (PowerState::A0PlusHP, true) => {
                    ringbuf_entry!(Trace::NICPowerEnableLow(pwren_l));
                    self.seq.set_bytes(Addr::NIC_CTRL, &[cld_rst]).unwrap();
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

                (PowerState::A2, _)
                | (PowerState::A2PlusMono, _)
                | (PowerState::A2PlusFans, _)
                | (PowerState::A1, _) => {
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

impl<S: SpiServer> ServerImpl<S> {
    fn update_state_internal(&mut self, state: PowerState) {
        ringbuf_entry!(Trace::UpdateState(state));
        self.state = state;
        self.jefe.set_state(state as u32);
    }

    fn set_state_internal(
        &mut self,
        state: PowerState,
    ) -> Result<(), SeqError> {
        ringbuf_entry!(Trace::SetState(self.state, state));

        ringbuf_entry!(Trace::PGStatus {
            b_pg: self.seq.read_byte(Addr::GROUPB_PG).unwrap(),
            c_pg: self.seq.read_byte(Addr::GROUPC_PG).unwrap(),
            nic: self.seq.read_byte(Addr::NIC_STATUS).unwrap(),
        });

        ringbuf_entry!(Trace::SMStatus {
            a1: self.seq.read_byte(Addr::A1SMSTATUS).unwrap(),
            a0: self.seq.read_byte(Addr::A0SMSTATUS).unwrap(),
        });

        ringbuf_entry!(Trace::PowerControl(
            self.seq.read_byte(Addr::PWR_CTRL).unwrap(),
        ));

        ringbuf_entry!(Trace::InterruptFlags(
            self.seq.read_byte(Addr::IFR).unwrap(),
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
                // We are going to pass through A1 on the way to A0.
                //
                let a1a0 = Reg::PWR_CTRL::A1PWREN | Reg::PWR_CTRL::A0A_EN;
                self.seq.write_bytes(Addr::PWR_CTRL, &[a1a0]).unwrap();
                let mut seen = Reg::A0SMSTATUS::Encoded::IDLE as u8;

                loop {
                    let mut power = [0u8, 0u8];

                    //
                    // We are going to read both the A1SMSTATUS and the
                    // A0SMSTATUS just so we can record the A1 state machine
                    // status -- but we only actually care about the A0 state
                    // machine.
                    //
                    const_assert!(Addr::A1SMSTATUS.precedes(Addr::A0SMSTATUS));
                    self.seq.read_bytes(Addr::A1SMSTATUS, &mut power).unwrap();
                    ringbuf_entry!(Trace::A1Power(power[0], power[1]));

                    if power[1] > seen {
                        //
                        // We have seen some surprising behavior with respect
                        // to rails appearing on before we have instructed
                        // them to do so.  To better understand these
                        // potential conditions, we record our rail status
                        // whenever we see our state machine advance.
                        //
                        vcore_soc_status();
                        seen = power[1];
                    }

                    if power[1] == Reg::A0SMSTATUS::Encoded::GROUPC_PG as u8 {
                        break;
                    }

                    hl::sleep_for(1);
                }

                //
                // And power up!
                //
                vcore_soc_on();
                ringbuf_entry!(Trace::RailsOn);
                vcore_soc_status();

                //
                // Now wait for the end of Group C.
                //
                loop {
                    let mut power = [0u8];

                    self.seq.read_bytes(Addr::A0SMSTATUS, &mut power).unwrap();
                    ringbuf_entry!(Trace::A0Power(power[0]));

                    if power[0] == Reg::A0SMSTATUS::Encoded::DONE as u8 {
                        break;
                    }

                    hl::sleep_for(1);
                }

                //
                // And establish our timer to check SP3_TO_SP_NIC_PWREN_L.
                //
                self.deadline = sys_get_timer().now + TIMER_INTERVAL;
                sys_set_timer(Some(self.deadline), notifications::TIMER_MASK);

                //
                // Finally, enable transmission to the SP3's UART
                //
                uart_sp_to_sp3_enable();
                ringbuf_entry!(Trace::UartEnabled);

                self.update_state_internal(PowerState::A0);
                Ok(())
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
                self.seq.set_bytes(Addr::NIC_CTRL, &[cld_rst]).unwrap();

                //
                // Start FPGA down-sequence. Clearing the enables immediately
                // de-asserts PWR_GOOD to the SP3 processor which the EDS
                // says is required before taking the rails out.
                // We also need to be headed down before the rails get taken out
                // so as not to trip a MAPO fault.
                //
                let a1a0 = Reg::PWR_CTRL::A1PWREN | Reg::PWR_CTRL::A0A_EN;
                self.seq.clear_bytes(Addr::PWR_CTRL, &[a1a0]).unwrap();

                //
                // FPGA de-asserts PWR_GOOD for 2 ms before yanking enables,
                // we wait for a tick here to make sure the SPI command to the
                // FPGA propagated and the FPGA has had time to act. AMD's EDS
                // doesn't give a minimum time so we'll give them 1 ms.
                //
                hl::sleep_for(1);
                vcore_soc_off();

                if self.hf.set_mux(hf_api::HfMuxState::SP).is_err() {
                    return Err(SeqError::MuxToSPFailed);
                }

                vcore_soc_status();
                self.update_state_internal(PowerState::A2);
                ringbuf_entry!(Trace::A2);

                Ok(())
            }

            _ => Err(SeqError::IllegalTransition),
        }
    }

    fn check_thermtrip(&mut self, ifr: u8) {
        let thermtrip = Reg::IFR::THERMTRIP;

        if ifr & thermtrip != 0 {
            self.seq.clear_bytes(Addr::IFR, &[thermtrip]).unwrap();
            self.update_state_internal(PowerState::A0Thermtrip);
        }
    }

    fn check_reset(&mut self, ifr: u8) {
        let pwrok_fedge = Reg::IFR::AMD_PWROK_FEDGE;

        if ifr & pwrok_fedge != 0 {
            let mut cnts = [0u8; 2];

            const_assert!(Addr::AMD_RSTN_CNTS.precedes(Addr::AMD_PWROKN_CNTS));
            self.seq.read_bytes(Addr::AMD_RSTN_CNTS, &mut cnts).unwrap();

            let (rstn, pwrokn) = (cnts[0], cnts[1]);
            ringbuf_entry!(Trace::ResetCounts { rstn, pwrokn });

            self.seq.write_bytes(Addr::AMD_RSTN_CNTS, &[0, 0]).unwrap();
            let mask = pwrok_fedge | Reg::IFR::AMD_RSTN_FEDGE;
            self.seq.clear_bytes(Addr::IFR, &[mask]).unwrap();
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
    ) -> Result<PowerState, RequestError<SeqError>> {
        Ok(self.state)
    }

    fn set_state(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
    ) -> Result<(), RequestError<SeqError>> {
        self.set_state_internal(state).map_err(RequestError::from)
    }

    fn fans_on(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SeqError>> {
        let on = Reg::EARLY_POWER_CTRL::FANPWREN;
        self.seq.set_bytes(Addr::EARLY_POWER_CTRL, &[on]).unwrap();
        Ok(())
    }

    fn fans_off(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SeqError>> {
        let off = Reg::EARLY_POWER_CTRL::FANPWREN;
        self.seq
            .clear_bytes(Addr::EARLY_POWER_CTRL, &[off])
            .unwrap();
        Ok(())
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
        target_board = "gimlet-a",
        target_board = "gimlet-b",
        target_board = "gimlet-c",
    ))] {
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

        // gimlet-a needs to have a pin flipped to mux the iCE40 SPI flash out
        // of circuit to be able to program the FPGA, because we accidentally
        // share a CS net between Flash and the iCE40.
        //
        // (port, mask, high_flag)
        #[cfg(target_board = "gimlet-a")]
        const FPGA_HACK_PINS: Option<&[(sys_api::PinSet, bool)]> = Some(&[
            // SEQ_TO_SEQ_MUX_SEL, pulled high, we drive it low
            (sys_api::Port::I.pin(8), false),
        ]);

        #[cfg(not(target_board = "gimlet-a"))]
        const FPGA_HACK_PINS: Option<&[(sys_api::PinSet, bool)]> = None;

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

        // Gimlet provides external pullups.
        const PGS_PULL: sys_api::Pull = sys_api::Pull::None;

        const NIC_PWREN_L_PINS: sys_api::PinSet = sys_api::Port::F.pin(4);

        // Externally pulled to V3P3_SYS_A0
        const NIC_PWREN_L_PULL: sys_api::Pull = sys_api::Pull::None;

        fn vcore_soc_off() {
            use drv_i2c_devices::raa229618::Raa229618;
            let i2c = I2C.get_task_id();

            let (device, rail) = i2c_config::pmbus::vdd_vcore(i2c);
            let mut vdd_vcore = Raa229618::new(&device, rail);

            let (device, rail) = i2c_config::pmbus::vddcr_soc(i2c);
            let mut vddcr_soc = Raa229618::new(&device, rail);

            vdd_vcore.turn_off().unwrap();
            vddcr_soc.turn_off().unwrap();
        }

        fn vcore_soc_on() {
            use drv_i2c_devices::raa229618::Raa229618;
            let i2c = I2C.get_task_id();

            let (device, rail) = i2c_config::pmbus::vdd_vcore(i2c);
            let mut vdd_vcore = Raa229618::new(&device, rail);

            let (device, rail) = i2c_config::pmbus::vddcr_soc(i2c);
            let mut vddcr_soc = Raa229618::new(&device, rail);

            vdd_vcore.turn_on().unwrap();
            vddcr_soc.turn_on().unwrap();
        }

        fn vcore_soc_status() {
            use drv_i2c_devices::raa229618::Raa229618;
            let i2c = I2C.get_task_id();

            let (device, rail) = i2c_config::pmbus::vdd_vcore(i2c);
            let mut vdd_vcore = Raa229618::new(&device, rail);

            match vdd_vcore.get_status() {
                Ok(status) => ringbuf_entry!(Trace::RailStatusCore(status)),
                Err(err) => ringbuf_entry!(Trace::RailErrorCore(err)),
            }

            let (device, rail) = i2c_config::pmbus::vddcr_soc(i2c);
            let mut vddcr_soc = Raa229618::new(&device, rail);

            match vddcr_soc.get_status() {
                Ok(status) => ringbuf_entry!(Trace::RailStatusSoc(status)),
                Err(err) => ringbuf_entry!(Trace::RailErrorSoc(err)),
            }
        }

    } else {
        compile_error!("unsupported target board");
    }
}

mod idl {
    use super::{PowerState, SeqError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
