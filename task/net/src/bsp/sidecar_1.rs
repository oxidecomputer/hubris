use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{self as sys_api, Sys};

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
use vsc85xx::{PhyRw, VscError};

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
    let mut bridge = MiimBridge { eth };
    // TODO: this may not be on port 0
    vsc85xx::init_vsc8552_phy(0, &mut bridge).unwrap();
}
