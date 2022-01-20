use crate::GPIO;
use drv_stm32h7_eth as eth;

pub fn configure_ethernet_pins() {
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

pub fn configure_phy(_eth: &mut eth::Ethernet) {}
