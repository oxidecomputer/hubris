// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::dice::{MfgResult, KEYCODE_LEN, KEY_INDEX, SEED_LEN};
use core::ops::{Deref, DerefMut};
use drv_lpc55_flash::{Flash, BYTES_PER_FLASH_PAGE};
use hubpack::SerializedSize;
use lib_dice::{
    CertSerialNumber, DiceMfg, IntermediateCert, PersistIdCert, PersistIdSeed,
    PlatformId, SeedBuf, SerialMfg,
};
use lib_lpc55_usart::Usart;
use lpc55_pac::Peripherals;
use lpc55_puf::Puf;
use salty::signature::Keypair;
use serde::{Deserialize, Serialize};
use static_assertions as sa;
use unwrap_lite::UnwrapLite;
use zeroize::Zeroizing;

macro_rules! flash_page_align {
    ($size:expr) => {
        if $size % BYTES_PER_FLASH_PAGE != 0 {
            ($size & !(BYTES_PER_FLASH_PAGE - 1)) + BYTES_PER_FLASH_PAGE
        } else {
            $size
        }
    };
}

// ensure DiceState object will fit in FLASH_DICE_MFG range
sa::const_assert!(
    (FLASH_DICE_MFG.end - FLASH_DICE_MFG.start) as usize
        >= flash_page_align!(DiceState::MAX_SIZE)
);

// ensure DICE_FLASH start and end are alligned
sa::const_assert!(
    (FLASH_DICE_MFG.end as usize).is_multiple_of(BYTES_PER_FLASH_PAGE)
);
sa::const_assert!(
    (FLASH_DICE_MFG.start as usize).is_multiple_of(BYTES_PER_FLASH_PAGE)
);

const VERSION: u32 = 0;
const MAGIC: [u8; 12] = [
    0x9e, 0xc8, 0x93, 0x2a, 0xb5, 0x51, 0x4a, 0x04, 0xd4, 0x43, 0x2c, 0x52,
];

#[derive(Debug, PartialEq)]
pub enum DiceStateError {
    Deserialize,
    Serialize,
}

#[derive(Deserialize, Serialize, SerializedSize)]
struct Header {
    pub version: u32,
    pub magic: [u8; 12],
}

impl Default for Header {
    fn default() -> Self {
        Self {
            version: VERSION,
            magic: MAGIC,
        }
    }
}

/// data received from manufacturing process
/// serialized to flash after mfg as device identity
#[derive(Deserialize, Serialize, SerializedSize)]
struct DiceState {
    pub persistid_key_code: [u32; KEYCODE_LEN],
    pub platform_id: PlatformId,
    pub persistid_cert: PersistIdCert,
    pub intermediate_cert: Option<IntermediateCert>,
}

impl DiceState {
    const ALIGNED_MAX_SIZE: usize =
        flash_page_align!(Header::MAX_SIZE + Self::MAX_SIZE);

    fn from_flash() -> Result<Self, DiceStateError> {
        // SAFETY: This unsafe block relies on the caller verifying that the
        // flash region being read has been programmed. We verify this in the
        // conditional evaluated before executing this unsafe code.
        let src = unsafe {
            core::slice::from_raw_parts(
                FLASH_DICE_MFG.start as *const u8,
                Self::ALIGNED_MAX_SIZE,
            )
        };

        let (header, rest) = hubpack::deserialize::<Header>(src)
            .map_err(|_| DiceStateError::Deserialize)?;

        if header.magic != MAGIC {
            panic!("DiceFlash bad magic");
        }
        if header.version != VERSION {
            panic!("DiceFlash bad version");
        }

        let (state, _) = hubpack::deserialize::<Self>(rest)
            .map_err(|_| DiceStateError::Deserialize)?;

        Ok(state)
    }

    pub fn to_flash(
        &self,
        flash: &mut Flash<'_>,
    ) -> Result<usize, DiceStateError> {
        let mut buf = [0u8; Self::ALIGNED_MAX_SIZE];

        let header = Header::default();
        let offset = hubpack::serialize(&mut buf, &header)
            .map_err(|_| DiceStateError::Serialize)?;

        let offset = hubpack::serialize(&mut buf[offset..], self)
            .map_err(|_| DiceStateError::Serialize)?;

        for (i, page) in buf.chunks_exact(BYTES_PER_FLASH_PAGE).enumerate() {
            let page: &[u8; BYTES_PER_FLASH_PAGE] =
                page.try_into().unwrap_lite();
            flash
                .write_page(
                    (FLASH_DICE_MFG.start as usize + i * BYTES_PER_FLASH_PAGE)
                        as u32,
                    page,
                    delay,
                )
                .expect("flash write");
        }

        Ok(offset)
    }

    pub fn is_programmed(flash: &mut Flash<'_>) -> bool {
        flash.is_page_range_programmed(
            FLASH_DICE_MFG.start,
            flash_page_align!(Header::MAX_SIZE + Self::MAX_SIZE) as u32,
        )
    }
}

fn delay() {
    // Timeouts timeouts etc, this just delays for a few cycles until
    // we check the flash again
    for _ in 0..100 {
        cortex_m::asm::nop();
    }
}

/// Generate platform identity key from PUF and manufacture the system
/// by certifying this identity. The certification process uses the usart
/// peripheral to exchange manufacturing data, CSR & cert with the
/// manufacturing line.
fn gen_artifacts_from_mfg(
    peripherals: &Peripherals,
    flash: &mut Flash<'_>,
) -> MfgResult {
    let puf = Puf::new(&peripherals.PUF);

    // Create key code for an ed25519 seed using the PUF. We use this seed
    // to generate a key used as an identity that is independent from the
    // DICE measured boot.
    let mut id_keycode = Zeroizing::new([0u32; KEYCODE_LEN]);
    if !puf.generate_keycode(KEY_INDEX, SEED_LEN, id_keycode.deref_mut()) {
        panic!("failed to generate key code");
    }
    let id_keycode = id_keycode;

    // get keycode from DICE MFG flash region
    // good opportunity to put a magic value in the DICE MFG flash region
    let mut seed = [0u8; SEED_LEN];
    if !puf.get_key(id_keycode.deref(), &mut seed) {
        panic!("failed to get ed25519 seed");
    }
    let seed = seed;

    // we're done with the puf: block the key index used for the identity
    // key and lock the block register
    if !puf.block_index(KEY_INDEX) {
        panic!("failed to block PUF index");
    }
    puf.lock_indices_low();

    let id_seed = PersistIdSeed::new(seed);

    let id_keypair = Keypair::from(id_seed.as_bytes());

    usart_setup(
        &peripherals.SYSCON,
        &peripherals.IOCON,
        &peripherals.FLEXCOMM0,
    );

    let usart = Usart::from(peripherals.USART0.deref());

    let dice_data =
        SerialMfg::new(&id_keypair, usart, &peripherals.SYSCON).run();

    let dice_state = DiceState {
        persistid_key_code: *id_keycode,
        platform_id: dice_data.platform_id,
        persistid_cert: dice_data.persistid_cert,
        intermediate_cert: dice_data.intermediate_cert,
    };

    dice_state.to_flash(flash).unwrap();

    MfgResult {
        cert_serial_number: Default::default(),
        platform_id: dice_state.platform_id,
        persistid_keypair: id_keypair,
        persistid_cert: dice_state.persistid_cert,
        intermediate_cert: dice_state.intermediate_cert,
    }
}

/// Get platform identity data from the DICE flash region. This is the data
/// we get from the 'gen_artifacts_from_mfg' function.
fn gen_artifacts_from_flash(peripherals: &Peripherals) -> MfgResult {
    let dice_state = DiceState::from_flash().expect("DiceState::from_flash");

    let puf = Puf::new(&peripherals.PUF);

    // get keycode from DICE MFG flash region
    let mut seed = [0u8; SEED_LEN];
    if !puf.get_key(&dice_state.persistid_key_code, &mut seed) {
        panic!("failed to get ed25519 seed");
    }
    let seed = seed;

    // we're done with the puf: block the key index used for the identity
    // key and lock the block register
    if !puf.block_index(KEY_INDEX) {
        panic!("failed to block PUF index");
    }
    puf.lock_indices_low();

    let id_seed = PersistIdSeed::new(seed);

    let id_keypair = Keypair::from(id_seed.as_bytes());

    MfgResult {
        cert_serial_number: CertSerialNumber::default(),
        platform_id: dice_state.platform_id,
        persistid_keypair: id_keypair,
        persistid_cert: dice_state.persistid_cert,
        intermediate_cert: dice_state.intermediate_cert,
    }
}

pub fn gen_mfg_artifacts_usart(
    peripherals: &Peripherals,
    flash: &mut Flash<'_>,
) -> MfgResult {
    if DiceState::is_programmed(flash) {
        gen_artifacts_from_flash(peripherals)
    } else {
        gen_artifacts_from_mfg(peripherals, flash)
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

include!(concat!(env!("OUT_DIR"), "/config.rs"));
