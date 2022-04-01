// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_sidecar_seq_api::Sequencer;
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use ringbuf::*;
use userlib::{hl::sleep_for, task_slot};
use vsc7448::{Vsc7448, Vsc7448Rw, VscError};
use vsc7448_pac::{phy, types::PhyRegisterAddress};
use vsc85xx::{vsc8504::Vsc8504, PhyRw};

task_slot!(SYS, sys);
task_slot!(NET, net);
task_slot!(SEQ, seq);

const MAC_SEEN_COUNT: usize = 64;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Vsc8504ModeStatus { port: u8, status: u16 },
    Vsc8504MacStatus { port: u8, status: u16 },
    MacAddress(vsc7448::mac::MacTableEntry),
    Vsc7448Error(VscError),
    Vsc8504Error(VscError),
}
ringbuf!(Trace, 16, Trace::None);

pub struct Bsp<'a, R> {
    vsc7448: &'a Vsc7448<'a, R>,
    vsc8504: Vsc8504,
    net: task_net_api::Net,
    known_macs: [Option<[u8; 6]>; MAC_SEEN_COUNT],
    vsc8504_mode_status: [u16; 4],
    vsc8504_mac_status: [u16; 4],
}

pub const REFCLK_SEL: vsc7448::RefClockFreq =
    vsc7448::RefClockFreq::Clk156p25MHz;
pub const REFCLK2_SEL: Option<vsc7448::RefClockFreq> = None;

/// For convenience, we implement `PhyRw` on a thin wrapper around the `net`
/// task handle.  We read and write to PHYs using RPC calls to the `net` task,
/// which owns the ethernet peripheral containing the MDIO block.
struct NetPhyRw<'a>(&'a mut task_net_api::Net);
impl<'a> PhyRw for NetPhyRw<'a> {
    #[inline(always)]
    fn read_raw<T: From<u16>>(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError> {
        self.0
            .smi_read(port, reg.addr)
            .map(|r| r.into())
            .map_err(|e| e.into())
    }

    #[inline(always)]
    fn write_raw<T>(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone,
    {
        self.0
            .smi_write(port, reg.addr, value.into())
            .map_err(|e| e.into())
    }
}

pub fn preinit() {
    // Wait for the sequencer to turn on the clock
    let seq = Sequencer::from(SEQ.get_task_id());
    while seq.is_clock_config_loaded().unwrap_or(0) == 0 {
        sleep_for(10);
    }
}

impl<'a, R: Vsc7448Rw> Bsp<'a, R> {
    /// Constructs and initializes a new BSP handle
    pub fn new(vsc7448: &'a Vsc7448<'a, R>) -> Result<Self, VscError> {
        let net = task_net_api::Net::from(NET.get_task_id());
        let mut out = Bsp {
            vsc7448,
            vsc8504: Vsc8504::empty(),
            net,
            known_macs: [None; MAC_SEEN_COUNT],
            vsc8504_mode_status: [0; 4],
            vsc8504_mac_status: [0; 4],
        };
        out.init()?;
        Ok(out)
    }

    fn init(&mut self) -> Result<(), VscError> {
        // Get a handle to modify GPIOs
        let sys = SYS.get_task_id();
        let sys = Sys::from(sys);

        // See RFD144 for a detailed look at the design
        self.vsc7448.init_sgmii(&[
            0,  // DEV1G_0   | SERDES1G_1  | Cubby 0
            1,  // DEV1G_1   | SERDES1G_2  | Cubby 1
            2,  // DEV1G_2   | SERDES1G_3  | Cubby 2
            3,  // DEV1G_3   | SERDES1G_4  | Cubby 3
            4,  // DEV1G_4   | SERDES1G_5  | Cubby 4
            5,  // DEV1G_5   | SERDES1G_6  | Cubby 5
            6,  // DEV1G_6   | SERDES1G_7  | Cubby 6
            7,  // DEV1G_7   | SERDES1G_8  | Cubby 7
            8,  // DEV2G5_0  | SERDES6G_0  | Cubby 8
            9,  // DEV2G5_1  | SERDES6G_1  | Cubby 9
            10, // DEV2G5_2  | SERDES6G_2  | Cubby 10
            11, // DEV2G5_3  | SERDES6G_3  | Cubby 11
            12, // DEV2G5_4  | SERDES6G_4  | Cubby 12
            13, // DEV2G5_5  | SERDES6G_5  | Cubby 13
            14, // DEV2G5_6  | SERDES6G_6  | Cubby 14
            15, // DEV2G5_7  | SERDES6G_7  | Cubby 15
            16, // DEV2G5_8  | SERDES6G_8  | Cubby 16
            17, // DEV2G5_9  | SERDES6G_9  | Cubby 17
            18, // DEV2G5_10 | SERDES6G_10 | Cubby 18
            19, // DEV2G5_11 | SERDES6G_11 | Cubby 19
            20, // DEV2G5_12 | SERDES6G_12 | Cubby 20
            21, // DEV2G5_13 | SERDES6G_13 | Cubby 21
            24, // DEV2G5_16 | SERDES6G_16 | Cubby 22
            25, // DEV2G5_17 | SERDES6G_17 | Cubby 23
            26, // DEV2G5_18 | SERDES6G_18 | Cubby 24
            27, // DEV2G5_19 | SERDES6G_19 | Cubby 25
            28, // DEV2G5_20 | SERDES6G_20 | Cubby 26
            29, // DEV2G5_21 | SERDES6G_21 | Cubby 27
            30, // DEV2G5_22 | SERDES6G_22 | Cubby 28
            31, // DEV2G5_23 | SERDES6G_23 | Cubby 29
            48, // DEV2G5_24 | SERDES1G_0  | Local SP
        ])?;
        self.vsc7448.init_10g_sgmii(&[
            51, // DEV2G5_27 | SERDES10G_2 | Cubby 30   (shadows DEV10G_2)
            52, // DEV2G5_28 | SERDES10G_3 | Cubby 31   (shadows DEV10G_3)
        ])?;

        self.phy_init(&sys)?;
        self.vsc7448.init_qsgmii(
            &[
                // Going to an on-board VSC8504 PHY (PHY4, U40), which is
                // configured over MIIM by the SP.
                //
                // 40 | DEV1G_16 | SERDES6G_14 | Peer SP
                // 41 | DEV1G_17 | SERDES6G_14 | PSC0
                // 42 | DEV1G_18 | SERDES6G_14 | PSC1
                // 43 | Unused
                40,
                // Going out to the front panel board, where there's a waiting
                // PHY that is configured by the FPGA.
                //
                // 44 | DEV1G_16 | SERDES6G_15 | Technician 0
                // 45 | DEV1G_17 | SERDES6G_15 | Technician 1
                // 42 | Unused
                // 43 | Unused
                44,
            ],
            vsc7448::Speed::Speed100M,
        )?;

        self.vsc7448.init_sfi(&[
            49, //  DEV10G_0 | SERDES10G_0 | Tofino 2
        ])?;

        self.vsc7448.apply_calendar()?;

        Ok(())
    }

    fn phy_init(&mut self, sys: &Sys) -> Result<(), VscError> {
        // Let's configure the on-board PHY first
        // Relevant pins are
        // - MIIM_SP_TO_PHY_MDC_2V5 (PC1)
        // - MIIM_SP_TO_PHY_MDIO_2V5 (PA2)
        // - MIIM_SP_TO_PHY_MDINT_2V5_L
        // - SP_TO_PHY4_COMA_MODE (PI10, internal pull-up)
        // - SP_TO_PHY4_RESET_L (PI9)
        //
        // The PHY talks on MIIM addresses 0x4-0x7 (configured by resistors
        // on the board)

        // TODO: wait for PLL lock to happen here
        use sys_api::*;

        let coma_mode = Port::I.pin(10);
        sys.gpio_set(coma_mode).unwrap();
        sys.gpio_configure_output(
            coma_mode,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        )
        .unwrap();
        sys.gpio_reset(coma_mode).unwrap();

        // Make NRST low then switch it to output mode
        let nrst = Port::I.pin(9);
        sys.gpio_reset(nrst).unwrap();
        sys.gpio_configure_output(
            nrst,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        )
        .unwrap();

        // Jiggle reset line, then wait 120 ms
        // SP_TO_LDO_PHY4_EN (PI6)
        let phy4_pwr_en = Port::I.pin(6);
        sys.gpio_reset(phy4_pwr_en).unwrap();
        sys.gpio_configure_output(
            phy4_pwr_en,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        )
        .unwrap();
        sys.gpio_reset(phy4_pwr_en).unwrap();
        sleep_for(10);

        // Power on!
        sys.gpio_set(phy4_pwr_en).unwrap();
        sleep_for(4);
        // TODO: sleep for PG lines going high here

        sys.gpio_set(nrst).unwrap();
        sleep_for(120); // Wait for the chip to come out of reset

        // Initialize the PHY, then disable COMA_MODE
        let rw = &mut NetPhyRw(&mut self.net);
        self.vsc8504 = Vsc8504::init(4, rw)?;

        // The VSC8504 on the sidecar has its SIGDET GPIOs pulled down,
        // for some reason.
        self.vsc8504.set_sigdet_polarity(rw, true).unwrap();

        sys.gpio_reset(coma_mode).unwrap();

        Ok(())
    }

    fn step(&mut self) {
        for port in 0..4 {
            let rw = &mut NetPhyRw(&mut self.net);
            let mut vsc8504 = self.vsc8504.phy(port, rw);
            match vsc8504.phy.read(phy::STANDARD::MODE_STATUS()) {
                Ok(status) => {
                    let status = u16::from(status);
                    if status != self.vsc8504_mode_status[port as usize] {
                        ringbuf_entry!(Trace::Vsc8504ModeStatus {
                            port,
                            status: u16::from(status)
                        });
                        self.vsc8504_mode_status[port as usize] = status;
                    }
                }
                Err(e) => ringbuf_entry!(Trace::Vsc8504Error(e)),
            };
            match vsc8504.phy.read(phy::EXTENDED_3::MAC_SERDES_PCS_STATUS()) {
                Ok(status) => {
                    let status = u16::from(status);
                    if status != self.vsc8504_mac_status[port as usize] {
                        ringbuf_entry!(Trace::Vsc8504MacStatus {
                            port,
                            status: u16::from(status)
                        });
                        self.vsc8504_mac_status[port as usize] = status;
                    }
                }
                Err(e) => ringbuf_entry!(Trace::Vsc8504Error(e)),
            };
        }

        // Dump the MAC tables
        loop {
            match vsc7448::mac::next_mac(self.vsc7448) {
                Ok(Some(mac)) => {
                    // Inefficient but easy way to avoid logging MAC addresses
                    // repeatedly.  This will fail to scale for larger systems,
                    // where we'd want some kind of LRU cache, but is nice
                    // for debugging.
                    let mut mac_is_new = true;
                    for m in self.known_macs.iter_mut() {
                        match m {
                            Some(m) => {
                                if *m == mac.mac {
                                    mac_is_new = false;
                                    break;
                                }
                            }
                            None => {
                                *m = Some(mac.mac);
                                break;
                            }
                        }
                    }
                    if mac_is_new {
                        ringbuf_entry!(Trace::MacAddress(mac));
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    ringbuf_entry!(Trace::Vsc7448Error(e));
                    break;
                }
            }
        }
    }

    pub fn run(&mut self) -> ! {
        loop {
            self.step();
            sleep_for(100);
        }
    }
}
