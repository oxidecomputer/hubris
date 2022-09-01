// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_auxflash_api::AuxFlashError;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};
use userlib::*;

/* Gimlet uses SPI instead
 *
 * SP_TO_FLASH_SPI_HOLD_N  PB8     GPIO
 * SP_TO_FLASH_SPI_CLK     PB13    SPI2_SCK
 * SP_TO_FLASH_SPI_MISO    PB14    SPI2_MISO
 * SP_TO_FLASH_SPI_MOSI    PB15    SPI2_MOSI
 * SP_TO_FLASH_SPI_CS      PB12    SPI2_NSS (or GPIO)
 * SP_TO_FLASH_SPI_WP_N    PB9     GPIO
 *
 * (This is `local_flash` in `gimlet/rev-b.toml`)
 */

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_stm32h7_qspi::Qspi;
use drv_stm32xx_sys_api as sys_api;

task_slot!(SYS, sys);
const QSPI_IRQ: u32 = 1;

////////////////////////////////////////////////////////////////////////////////

/// Simple handle which holds a `&Qspi` and allows us to implement `TlvcRead`
#[derive(Copy, Clone)]
struct QspiTlvcHandle<'a>(&'a Qspi);

impl<'a> tlvc::TlvcRead for QspiTlvcHandle<'a> {
    fn extent(&self) -> Result<u64, tlvc::TlvcReadError> {
        // TODO this is hard-coded for the Sidecar rev A flash
        Ok(1 << 24)
    }
    fn read_exact(
        &self,
        offset: u64,
        dest: &mut [u8],
    ) -> Result<(), tlvc::TlvcReadError> {
        Ok(self.0.read_memory(offset.try_into().unwrap_lite(), dest))
    }
}

////////////////////////////////////////////////////////////////////////////////

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.enable_clock(sys_api::Peripheral::QuadSpi);
    sys.leave_reset(sys_api::Peripheral::QuadSpi);

    let reg = unsafe { &*device::QUADSPI::ptr() };
    let qspi = Qspi::new(reg, QSPI_IRQ);

    let clock = 5; // 200MHz kernel / 5 = 40MHz clock
    qspi.configure(clock, 24); // 2**24 = 16MiB = 128Mib

    // Sidecar-only for now!
    //
    // This is mostly copied from `gimlet-hf-server`, with a few pin adjustments
    //
    // SP_QSPI_RESET_L     PF5     GPIO
    // SP_QSPI_CLK         PF10    QUADSPI_CLK
    // SP_QSPI_IO0 (SI)    PF8     QUADSPI_BK1_IO0
    // SP_QSPI_IO1 (SO)    PF9     QUADSPI_BK1_IO1
    // SP_QSPI_CS_L        PG6     QUADSPI_BK1_NCS (or GPIO?)
    // SP_QSPI_IO2 (*WP)   PF7     QUADSPI_BK1_IO2
    // SP_QSPI_IO3 (*HOLD) PF6     QUADSPI_BK1_IO3
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(6).and_pin(7).and_pin(10),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF9,
    )
    .unwrap();
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(8).and_pin(9),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    )
    .unwrap();
    sys.gpio_configure_alternate(
        sys_api::Port::G.pin(6),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    )
    .unwrap();

    let qspi_reset = sys_api::Port::F.pin(5);
    sys.gpio_reset(qspi_reset).unwrap();
    sys.gpio_configure_output(
        qspi_reset,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    )
    .unwrap();

    // TODO: The best clock frequency to use can vary based on the flash
    // part, the command used, and signal integrity limits of the board.

    // Ensure hold time for reset in case we just restarted.
    // TODO look up actual hold time requirement
    hl::sleep_for(1);

    // Release reset and let it stabilize.
    sys.gpio_set(qspi_reset).unwrap();
    hl::sleep_for(10);

    // TODO: check the ID and make sure it's what we expect
    //
    // Gimlet is  MT25QU256ABA8E12
    // Sidecar is S25FL128SAGMFIR01
    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl { qspi };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl {
    qspi: Qspi,
}

impl ServerImpl {
    fn poll_for_write_complete(&self) {
        loop {
            let status = self.qspi.read_status();
            if status & 1 == 0 {
                // ooh we're done
                break;
            }
        }
    }

    fn set_and_check_write_enable(&self) -> Result<(), AuxFlashError> {
        self.qspi.write_enable();
        let status = self.qspi.read_status();

        if status & 0b10 == 0 {
            // oh oh
            return Err(AuxFlashError::WriteEnableFailed);
        }
        Ok(())
    }
}

impl idl::InOrderAuxFlashImpl for ServerImpl {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 20], RequestError<AuxFlashError>> {
        let mut idbuf = [0; 20];
        self.qspi.read_id(&mut idbuf);
        Ok(idbuf)
    }

    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<AuxFlashError>> {
        Ok(self.qspi.read_status())
    }

    fn read_slot_chck(
        &mut self,
        _: &RecvMessage,
        slot: u32,
    ) -> Result<[u32; 4], RequestError<AuxFlashError>> {
        Ok([0; 4])
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::AuxFlashError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
