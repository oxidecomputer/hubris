use crate::GPIO;
use drv_stm32h7_eth as eth;

/// Address used on the MDIO link by our Ethernet PHY. Different
/// vendors have different defaults for this, it will likely need to
/// become configurable.
const PHYADDR: u8 = 0x01;

pub struct Bsp {
    // Empty handle
}

impl Bsp {
    pub fn new() -> Self {
        Self {}
    }
    pub fn configure_ethernet_pins(&self) {
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
        // RMII TXD1        PB13 <-- port B
        // RMII TXD0        PG13
        use drv_stm32h7_gpio_api::*;

        let gpio = Gpio::from(GPIO.get_task_id());
        let eth_af = Alternate::AF11;

        gpio.configure(
            Port::A,
            (1 << 1) | (1 << 2) | (1 << 7),
            Mode::Alternate,
            OutputType::PushPull,
            Speed::VeryHigh,
            Pull::None,
            eth_af,
        )
        .unwrap();
        gpio.configure(
            Port::B,
            1 << 13,
            Mode::Alternate,
            OutputType::PushPull,
            Speed::VeryHigh,
            Pull::None,
            eth_af,
        )
        .unwrap();
        gpio.configure(
            Port::C,
            (1 << 1) | (1 << 4) | (1 << 5),
            Mode::Alternate,
            OutputType::PushPull,
            Speed::VeryHigh,
            Pull::None,
            eth_af,
        )
        .unwrap();
        gpio.configure(
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

    pub fn wake(&self, _eth: &mut eth::Ethernet) {
        // Nothing to do here
    }

    pub fn configure_phy(&self, eth: &mut eth::Ethernet) {
        // Set up the PHY.
        let mii_basic_control =
            eth.smi_read(PHYADDR, eth::SmiClause22Register::Control);
        let mii_basic_control = mii_basic_control
        | 1 << 12 // AN enable
        | 1 << 9 // restart autoneg
        ;
        eth.smi_write(
            PHYADDR,
            eth::SmiClause22Register::Control,
            mii_basic_control,
        );

        // Wait for link-up
        while eth.smi_read(PHYADDR, eth::SmiClause22Register::Status) & (1 << 2)
            == 0
        {
            userlib::hl::sleep_for(1);
        }
    }
}
