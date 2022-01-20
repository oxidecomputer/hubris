use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use userlib::hl::sleep_for;

pub fn configure_ethernet_pins(sys: &Sys) {
    // This board's mapping:
    //
    // RMII REF CLK     PA1
    // MDIO             PA2
    // RMII RX DV       PA7
    //
    // MDC              PC1
    // RMII RXD0        PC4
    // RMII RXD1        PC5
    //
    // RMII TX EN       PG11
    // RMII TXD1        PG12
    // RMII TXD0        PG13
    //
    // (it's _almost_ identical to the STM32H7 Nucleo, except that
    //  TXD1 is on a different pin)
    use sys_api::*;

    let eth_af = Alternate::AF11;

    sys.gpio_configure(
        Port::A,
        (1 << 1) | (1 << 2) | (1 << 7),
        Mode::Alternate,
        OutputType::PushPull,
        Speed::VeryHigh,
        Pull::None,
        eth_af,
    )
    .unwrap();
    sys.gpio_configure(
        Port::C,
        (1 << 1) | (1 << 4) | (1 << 5),
        Mode::Alternate,
        OutputType::PushPull,
        Speed::VeryHigh,
        Pull::None,
        eth_af,
    )
    .unwrap();
    sys.gpio_configure(
        Port::G,
        (1 << 11) | (1 << 12) | (1 << 13),
        Mode::Alternate,
        OutputType::PushPull,
        Speed::VeryHigh,
        Pull::None,
        eth_af,
    )
    .unwrap();
}

use vsc7448_pac::types::PhyRegisterAddress;
use vsc85xx::{Phy, PhyRw, VscError};

/// Helper struct to implement the `PhyRw` trait using direct access through
/// `eth`'s MIIM registers.
struct MiimBridge<'a> {
    eth: &'a mut eth::Ethernet,
}
impl PhyRw for MiimBridge<'_> {
    fn read_raw<T: From<u16>>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError> {
        Ok(self.eth.smi_read(phy, reg.addr).into())
    }
    fn write_raw<T>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone,
    {
        self.eth.smi_write(phy, reg.addr, value.into());
        Ok(())
    }
}

pub fn configure_phy(eth: &mut eth::Ethernet) {
    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = Gpio::from(gpio_driver);

    // SP_TO_LDO_PHY2_EN (PI11)
    let phy2_pwr_en = gpio_api::Port::I.pin(11);
    gpio_driver.reset(phy2_pwr_en).unwrap();
    gpio_driver
        .configure_output(
            phy2_pwr_en,
            gpio_api::OutputType::PushPull,
            gpio_api::Speed::Low,
            gpio_api::Pull::None,
        )
        .unwrap();
    gpio_driver.set(phy2_pwr_en).unwrap();
    sleep_for(10); // TODO: how long does this need to be?

    // - SP_TO_PHY2_COMA_MODE (PI15, internal pull-up)
    // - SP_TO_PHY2_RESET_3V3_L (PI14)
    let coma_mode = gpio_api::Port::I.pin(15);
    gpio_driver.set(coma_mode).unwrap();
    gpio_driver
        .configure_output(
            coma_mode,
            gpio_api::OutputType::PushPull,
            gpio_api::Speed::Low,
            gpio_api::Pull::None,
        )
        .unwrap();

    let nrst = gpio_api::Port::I.pin(14);
    gpio_driver.reset(nrst).unwrap();
    gpio_driver
        .configure_output(
            nrst,
            gpio_api::OutputType::PushPull,
            gpio_api::Speed::Low,
            gpio_api::Pull::None,
        )
        .unwrap();
    sleep_for(10);
    gpio_driver.set(nrst).unwrap();
    sleep_for(120); // Wait for the chip to come out of reset

    // This PHY is on MIIM ports 0 and 1, based on resistor strapping
    let mut phy_rw = MiimBridge { eth };
    let mut phy = Phy {
        port: 0,
        rw: &mut phy_rw,
    };
    vsc85xx::init_vsc8552_phy(&mut phy).unwrap();

    // Disable COMA_MODE
    gpio_driver.reset(coma_mode).unwrap();
}
