// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Grapefruit FPGA process.

#![no_std]
#![no_main]

use drv_cpu_seq_api::{PowerState, StateChangeReason};
use drv_spi_api::{SpiDevice, SpiServer};
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{NotificationHandler, RequestError};
use task_jefe_api::Jefe;
use task_packrat_api::{CacheSetError, MacAddressBlock, Packrat, VpdIdentity};
use userlib::{
    hl, sys_recv_notification, task_slot, FromPrimitive, RecvMessage,
    UnwrapLite,
};

use ringbuf::{counted_ringbuf, ringbuf_entry, Count};

task_slot!(JEFE, jefe);

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    FpgaInit,
    StartFailed(#[count(children)] SeqError),
    ContinueBitstreamLoad(usize),
    WaitForDone,
    Programmed,
    MacsAlreadySet(MacAddressBlock),
    IdentityAlreadySet(VpdIdentity),

    #[count(skip)]
    None,
}

#[derive(Copy, Clone, PartialEq, Count)]
enum SeqError {
    AuxFlashError(#[count(children)] drv_auxflash_api::AuxFlashError),
    AuxChecksumMismatch,
    FpgaError(#[count(children)] drv_spartan7_spi_program::Spartan7Error),
}

impl From<drv_auxflash_api::AuxFlashError> for SeqError {
    fn from(v: drv_auxflash_api::AuxFlashError) -> Self {
        SeqError::AuxFlashError(v)
    }
}

impl From<drv_spartan7_spi_program::Spartan7Error> for SeqError {
    fn from(v: drv_spartan7_spi_program::Spartan7Error) -> Self {
        SeqError::FpgaError(v)
    }
}

counted_ringbuf!(Trace, 128, Trace::None);

task_slot!(SYS, sys);
task_slot!(SPI, spi);
task_slot!(AUXFLASH, auxflash);
task_slot!(PACKRAT, packrat);

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let spi = drv_spi_api::Spi::from(SPI.get_task_id());
    let aux = drv_auxflash_api::AuxFlash::from(AUXFLASH.get_task_id());

    // Populate packrat with dummy values, because talking to the EEPROM is hard
    let packrat = Packrat::from(PACKRAT.get_task_id());
    let macs = MacAddressBlock {
        base_mac: [0; 6],
        count: 0.into(),
        stride: 0,
    };
    match packrat.set_mac_address_block(macs) {
        Ok(()) => (),
        Err(CacheSetError::ValueAlreadySet) => {
            ringbuf_entry!(Trace::MacsAlreadySet(macs));
        }
    }
    let identity = VpdIdentity {
        serial: *b"GRAPEFRUIT\0",
        part_number: *b"913-0000083",
        revision: 0,
    };
    match packrat.set_identity(identity) {
        Ok(()) => (),
        Err(CacheSetError::ValueAlreadySet) => {
            ringbuf_entry!(Trace::IdentityAlreadySet(identity));
        }
    }

    match ServerImpl::init(&sys, spi, aux) {
        // Set up everything nicely, time to start serving incoming messages.
        Ok(mut server) => {
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

#[allow(unused)]
struct ServerImpl<S: SpiServer> {
    jefe: Jefe,
    sys: sys_api::Sys,
    seq: SpiDevice<S>,
}

const FAULT_PIN_L: sys_api::PinSet = sys_api::Port::A.pin(15);

const FPGA_PROGRAM_L: sys_api::PinSet = sys_api::Port::B.pin(6);
const FPGA_INIT_L: sys_api::PinSet = sys_api::Port::B.pin(5);
const FPGA_CONFIG_DONE: sys_api::PinSet = sys_api::Port::B.pin(4);

const FPGA_LOGIC_RESET_L: sys_api::PinSet = sys_api::Port::I.pin(15);

impl<S: SpiServer + Clone> ServerImpl<S> {
    fn init(
        sys: &sys_api::Sys,
        spi: S,
        aux: drv_auxflash_api::AuxFlash,
    ) -> Result<Self, SeqError> {
        // Ensure the SP fault pin is configured as an open-drain output, and
        // pull it low to make the sequencer restart externally visible.
        sys.gpio_configure_output(
            FAULT_PIN_L,
            sys_api::OutputType::OpenDrain,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );
        sys.gpio_reset(FAULT_PIN_L);

        // Hold the user logic in reset until we've loaded the bitstream
        sys.gpio_reset(FPGA_LOGIC_RESET_L);
        sys.gpio_configure_output(
            FPGA_LOGIC_RESET_L,
            sys_api::OutputType::PushPull,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );

        // Bind to the sequencer device on our SPI port
        let seq = spi.device(drv_spi_api::devices::FPGA);

        let pin_cfg = drv_spartan7_spi_program::Config {
            program_l: FPGA_PROGRAM_L,
            init_l: FPGA_INIT_L,
            config_done: FPGA_CONFIG_DONE,
        };
        ringbuf_entry!(Trace::FpgaInit);
        let loader =
            drv_spartan7_spi_program::BitstreamLoader::begin_bitstream_load(
                sys, &pin_cfg, &seq, true,
            )?;

        let sha_out = aux.get_compressed_blob_streaming(
            *b"FPGA",
            |chunk| -> Result<(), SeqError> {
                loader.continue_bitstream_load(chunk)?;
                ringbuf_entry!(Trace::ContinueBitstreamLoad(chunk.len()));
                Ok(())
            },
        )?;

        if sha_out != gen::FPGA_BITSTREAM_CHECKSUM {
            // Reset the FPGA to clear the invalid bitstream
            sys.gpio_reset(FPGA_PROGRAM_L);
            hl::sleep_for(1);
            sys.gpio_set(FPGA_PROGRAM_L);

            return Err(SeqError::AuxChecksumMismatch);
        }

        ringbuf_entry!(Trace::WaitForDone);
        loader.finish_bitstream_load()?;

        ringbuf_entry!(Trace::Programmed);

        let server = Self {
            sys: sys.clone(),
            jefe: Jefe::from(JEFE.get_task_id()),
            seq,
        };
        server.set_state_impl(PowerState::A2);

        // Clear the external fault now that we're about to start serving
        // messages and fewer things can go wrong.
        sys.gpio_set(FAULT_PIN_L);

        // Enable the user design
        sys.gpio_set(FPGA_LOGIC_RESET_L);

        Ok(server)
    }

    fn get_state_impl(&self) -> PowerState {
        // Only we should be setting the state, and we set it to A2 on startup;
        // this conversion should never fail.
        PowerState::from_u32(self.jefe.get_state()).unwrap_lite()
    }

    fn set_state_impl(&self, state: PowerState) {
        self.jefe.set_state(state as u32);
    }

    fn validate_state_change(
        &self,
        state: PowerState,
    ) -> Result<(), drv_cpu_seq_api::SeqError> {
        match (self.get_state_impl(), state) {
            (PowerState::A2, PowerState::A0)
            | (PowerState::A0, PowerState::A2)
            | (PowerState::A0PlusHP, PowerState::A2)
            | (PowerState::A0Thermtrip, PowerState::A2) => Ok(()),

            _ => Err(drv_cpu_seq_api::SeqError::IllegalTransition),
        }
    }
}

// The `Sequencer` implementation for Grapefruit is copied from
// `mock-gimlet-seq-server`.  State is set to Jefe, but isn't actually
// controlled here.
impl<S: SpiServer + Clone> idl::InOrderSequencerImpl for ServerImpl<S> {
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
    ) -> Result<(), RequestError<drv_cpu_seq_api::SeqError>> {
        self.validate_state_change(state)?;
        self.set_state_impl(state);
        Ok(())
    }

    fn set_state_with_reason(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
        _: StateChangeReason,
    ) -> Result<(), RequestError<drv_cpu_seq_api::SeqError>> {
        self.validate_state_change(state)?;
        self.set_state_impl(state);
        Ok(())
    }

    fn send_hardware_nmi(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        let ptr = reg::sgpio::OUT1;
        // SAFETY: the FPGA must be loaded, and these registers are in our FMC
        // region, so we can access them.
        unsafe {
            let orig = ptr.read_volatile();
            ptr.write_volatile(
                (orig & !reg::sgpio::out1::MGMT_ASSERT_NMI_BTN_L) & 0xFFFF,
            );
            hl::sleep_for(1000);
            ptr.write_volatile(orig);
        }
        Ok(())
    }

    fn read_fpga_regs(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 64], RequestError<core::convert::Infallible>> {
        Ok([0; 64])
    }
}

impl<S: SpiServer> NotificationHandler for ServerImpl<S> {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

/// Register map for SGPIO registers
#[allow(unused)]
mod reg {
    pub const BASE: *mut u32 = 0x60000000 as *mut _;

    pub const SGPIO: *mut u32 = BASE.wrapping_add(0xc0);
    pub mod sgpio {
        use super::*;
        pub const OUT0: *mut u32 = SGPIO.wrapping_add(0x0);
        pub const IN0: *mut u32 = SGPIO.wrapping_add(0x1);
        pub const OUT1: *mut u32 = SGPIO.wrapping_add(0x2);
        pub const IN1: *mut u32 = SGPIO.wrapping_add(0x3);
        pub mod out0 {
            pub const HAWAII_HEARTBEAT: u32 = 1 << 14;
            pub const MB_SCM_HPM_STBY_RDY: u32 = 1 << 14;
            pub const HPM_BMC_GPIOY3: u32 = 1 << 11;
            pub const MGMT_SMBUS_DATA: u32 = 1 << 10;
            pub const MGMT_SMBUS_CLK: u32 = 1 << 9;
            pub const GPIO_OUTPUT_9: u32 = 1 << 8;
            pub const GPIO_OUTPUT_8: u32 = 1 << 7;
            pub const GPIO_OUTPUT_7: u32 = 1 << 6;
            pub const GPIO_OUTPUT_6: u32 = 1 << 5;
            pub const BMC_READY: u32 = 1 << 4;
            pub const HPM_BMC_GPIOL5: u32 = 1 << 3;
            pub const HPM_BMC_GPIOL4: u32 = 1 << 2;
            pub const HPM_BMC_GPIOH3: u32 = 1 << 1;
            pub const MGMT_ASSERT_LOCAL_LOCK: u32 = 1 << 0;
        }
        pub mod in0 {
            pub const BMC_SCM_FPGA_UART_RX: u32 = 1 << 15;
            pub const MGMT_SYS_MON_PWR_GOOD: u32 = 1 << 14;
            pub const MGMT_SYS_MON_NMI_BTN_L: u32 = 1 << 13;
            pub const MGMT_SYS_MON_PWR_BTN_L: u32 = 1 << 12;
            pub const MGMT_SYS_MON_RST_BTN_L: u32 = 1 << 11;
            pub const DEBUG_INPUT1: u32 = 1 << 10;
            pub const MGMT_AC_LOSS_L: u32 = 1 << 9;
            pub const MGMT_SYS_MON_ATX_PWR_OK: u32 = 1 << 8;
            pub const MGMT_SYS_MON_P1_THERMTRIP_L: u32 = 1 << 7;
            pub const MGMT_SYS_MON_P0_THERMTRIP_L: u32 = 1 << 6;
            pub const MGMT_SYS_MON_P1_PROCHOT_L: u32 = 1 << 5;
            pub const MGMT_SYS_MON_P0_PROCHOT_L: u32 = 1 << 4;
            pub const MGMT_SYS_MON_RESET_L: u32 = 1 << 3;
            pub const P1_PRESENT_L: u32 = 1 << 2;
            pub const P0_PRESENT_L: u32 = 1 << 1;
            pub const MGMT_SYS_MON_POST_COMPLETE: u32 = 1 << 0;
        }
        pub mod out1 {
            pub const BMC_SCM_FPGA_UART_TX: u32 = 1 << 14;
            pub const MGMT_ASSERT_NMI_BTN_L: u32 = 1 << 13;
            pub const MGMT_ASSERT_PWR_BTN_L: u32 = 1 << 12;
            pub const MGMT_ASSERT_RST_BTN_L: u32 = 1 << 11;
            pub const JTAG_TRST_N: u32 = 1 << 10;
            pub const GPIO_OUTPUT_5: u32 = 1 << 9;
            pub const GPIO_OUTPUT_4: u32 = 1 << 8;
            pub const GPIO_OUTPUT_3: u32 = 1 << 7;
            pub const GPIO_OUTPUT_2: u32 = 1 << 6;
            pub const GPIO_OUTPUT_1: u32 = 1 << 5;
            pub const MGMT_ASSERT_CLR_CMOS: u32 = 1 << 4;
            pub const MGMT_ASSERT_P1_PROCHOT: u32 = 1 << 3;
            pub const MGMT_ASSERT_P0_PROCHOT: u32 = 1 << 2;
            pub const MGMT_SOC_RESET_L: u32 = 1 << 1;
            pub const MGMT_ASERT_WARM_RST_BTN_L: u32 = 1 << 0;
        }
        pub mod in1 {
            pub const MGMT_SMBUS_ALERT_L: u32 = 1 << 15;
            pub const HPM_BMC_GPIOI7: u32 = 1 << 14;
            pub const ESPI_BOOT_SEL: u32 = 1 << 13;
            pub const I2C_BMC_MB_ALERT_S: u32 = 1 << 12;
            pub const GPIO_INPUT_6: u32 = 1 << 8;
            pub const GPIO_INPUT_5: u32 = 1 << 7;
            pub const GPIO_INPUT_4: u32 = 1 << 6;
            pub const GPIO_INPUT_3: u32 = 1 << 5;
            pub const GPIO_INPUT_2: u32 = 1 << 4;
            pub const GPIO_INPUT_1: u32 = 1 << 3;
            pub const HPM_BMC_GPIOM5: u32 = 1 << 2;
            pub const HPM_BMC_GPIOM4: u32 = 1 << 1;
            pub const HPM_BMC_GPIOM3: u32 = 1 << 0;
        }
    }
}

mod idl {
    use drv_cpu_seq_api::{SeqError, StateChangeReason};
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod gen {
    include!(concat!(env!("OUT_DIR"), "/grapefruit_fpga.rs"));
}
