// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::dice::SerialNumbers;
use core::ops::Deref;
use dice_crate::{CertData, DeviceIdSerialMfg, DiceMfg, Handoff};
use lib_lpc55_usart::Usart;
use lpc55_pac::Peripherals;
use salty::signature::Keypair;

pub fn gen_mfg_artifacts(
    deviceid_keypair: &Keypair,
    peripherals: &Peripherals,
    handoff: &Handoff,
) -> SerialNumbers {
    usart_setup(
        &peripherals.SYSCON,
        &peripherals.IOCON,
        &peripherals.FLEXCOMM0,
    );

    let usart = Usart::from(peripherals.USART0.deref());
    let mfg_state = DeviceIdSerialMfg::new(&deviceid_keypair, usart).run();

    let cert_data =
        CertData::new(mfg_state.deviceid_cert, mfg_state.intermediate_cert);
    handoff.store(&cert_data);

    SerialNumbers {
        cert_serial_number: mfg_state.cert_serial_number,
        serial_number: mfg_state.serial_number,
    }
}

pub fn usart_setup(
    syscon: &lpc55_pac::syscon::RegisterBlock,
    iocon: &lpc55_pac::iocon::RegisterBlock,
    flexcomm: &lpc55_pac::flexcomm0::RegisterBlock,
) {
    gpio_setup(syscon, iocon);
    flexcomm0_setup(syscon, flexcomm);
}

/// Configure GPIO pin 29 & 30 for RX & TX respectively, as well as
/// digital mode.
fn gpio_setup(
    syscon: &lpc55_pac::syscon::RegisterBlock,
    iocon: &lpc55_pac::iocon::RegisterBlock,
) {
    // IOCON: enable clock & reset
    syscon.ahbclkctrl0.modify(|_, w| w.iocon().enable());
    syscon.presetctrl0.modify(|_, w| w.iocon_rst().released());

    // GPIO: enable clock & reset
    syscon.ahbclkctrl0.modify(|_, w| w.gpio0().enable());
    syscon.presetctrl0.modify(|_, w| w.gpio0_rst().released());

    // configure GPIO pin 29 & 30 for RX & TX respectively, as well as
    // digital mode
    iocon
        .pio0_29
        .write(|w| w.func().alt1().digimode().digital());
    iocon
        .pio0_30
        .write(|w| w.func().alt1().digimode().digital());

    // disable IOCON clock
    syscon.ahbclkctrl0.modify(|_, w| w.iocon().disable());
}

fn flexcomm0_setup(
    syscon: &lpc55_pac::syscon::RegisterBlock,
    flexcomm: &lpc55_pac::flexcomm0::RegisterBlock,
) {
    syscon.ahbclkctrl1.modify(|_, w| w.fc0().enable());
    syscon.presetctrl1.modify(|_, w| w.fc0_rst().released());

    // Set flexcom to be a USART
    flexcomm.pselid.write(|w| w.persel().usart());

    // set flexcomm0 / uart clock to 12Mhz
    syscon.fcclksel0().modify(|_, w| w.sel().enum_0x2());
}
