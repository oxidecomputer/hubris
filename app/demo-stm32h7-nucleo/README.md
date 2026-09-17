# STM32H7 Nucleo-144 Demo Application

Hubris application configurations for the STM32H7 Nucleo-144
development boards:

- `app-h743.toml` -- NUCLEO-H743ZI (order code NUCLEO-H743ZI2)
- `app-h753.toml` -- NUCLEO-H753ZI

The Nucleo boards are readily available development boards for general
Hubris development. Although they are not part of any Oxide Computer
product, they are configured to work with the Management Gateway Service
to allow over-the-network update and testing.

## Board differences

Both boards use the same Nucleo-144 form factor and share identical
pin assignments. The STM32H753 includes a hardware hash accelerator
supported by Hubris that the STM32H743 lacks.

## VLAN

Unlike Oxide designed boards, the Nucleo boards do not have a KSZ8463
Ethernet switch chip and have no need for 802.1q VLAN tags.  The `vlan`
feature is omitted through conditional compilation, i.e. `#[cfg(feature =
"vlan")]` and does not affect production code.

## Pin assignments

[Reference: UM2407 Rev 2, "STM32H7 Nucleo-144 boards (MB1364)"](https://www.st.com/resource/en/user_manual/um2407-stm32h7-nucleo144-boards-mb1364-stmicroelectronics.pdf)):

### Ethernet (RMII, active)

| Pin  | Function        | Connector        |
|------|-----------------|------------------|
| PA1  | RMII Ref Clock  | --               |
| PA2  | RMII MDIO       | --               |
| PA7  | RMII RX DV      | --               |
| PC1  | RMII MDC        | --               |
| PC4  | RMII RXD0       | --               |
| PC5  | RMII RXD1       | --               |
| PG11 | RMII TX Enable  | --               |
| PG13 | RMII TXD0       | --               |
| PB13 | RMII TXD1       | CN7 pin 5 (Zio) |

### SPI3 -- SP-to-RoT connection (active, no peer)

In the past, developers have connected an NXP LPC55S69 xPresso board to
emulate an RoT. We do not make any effort to support this configuration.
It is more complicated to get it right than just using jumper wires.

We still reserve the pins to make sure that we can run the CPA and its
required `sprot` task without modifications or interfering with other
signals. In this configuration, the non-existent "RoT" just won't respond.


| Pin  | Function  | AF | Connector                  |
|------|-----------|----|----------------------------|
| PC10 | SPI3_SCK  | 6  | CN11 pin 1, CN8 pin 6      |
| PC11 | SPI3_MISO | 6  | CN11 pin 2, CN8 pin 8      |
| PC12 | SPI3_MOSI | 6  | CN11 pin 3, CN8 pin 10     |
| PA15 | SPI3_NSS  | -- | CN11 pin 17, CN7 pin 9     |

The Zio connector labels these as SDMMC_D2/D3/CK (CN8).  SDMMC is
not used.  The SPI3 alternate function is selected in firmware.

### GPIO -- RoT signals

| Pin  | Function    | Direction | Connector   |
|------|-------------|-----------|-------------|
| PD0  | rot_irq     | Input     | CN11 pin 57 |
| PE6  | sprot debug | Output    | CN11 pin 62 |

PD0 is labeled CAN_RX on the Zio connector (CN9 pin 25); CAN is not
used.  PE6 is labeled SAI_A_SD (CN9 pin 20); SAI is not used.

### SPI1 -- General purpose header

| Pin  | Function   | AF | H743         | H753         |
|------|------------|----|--------------|--------------|
| PA5  | SPI1_SCK   | 5  | Used         | --           |
| PA3  | SPI1_SCK   | 5  | --           | Used         |
| PB5  | SPI1_MOSI  | 5  | Used         | Used         |
| PA6  | SPI1_MISO  | 5  | Used         | Used         |
| PD14 | SPI1_CS    | -- | Used         | Used         |

### I2C2

| Pin | Function  | AF |
|-----|-----------|----|
| PF1 | I2C2_SCL  | 4  |
| PF0 | I2C2_SDA  | 4  |

### USART1 -- Host UART (active, no peer)

| Pin  | Function   | Connector   |
|------|------------|-------------|
| PA9  | USART1_TX  | CN11 pin 21 |
| PA10 | USART1_RX  | CN11 pin 33 |

Reserved by CPA for host-SP communication.  No host CPU on the
Nucleo; the UART is idle.

### On-board hardware (do not reassign)

| Pin  | Function                | Notes                   |
|------|-------------------------|-------------------------|
| PC13 | User button B1          | GPIO IRQ                |
| PB0  | User LED LD1 (green)    | Default SB39/SB47 config|
| PE1  | User LED LD2 (yellow)   | --                      |
| PB14 | User LED LD3 (red)      | --                      |
| PA13 | ST-Link SWDIO           | Do not use as GPIO      |
| PA14 | ST-Link SWCLK           | Do not use as GPIO      |
| PD8  | USART3 TX (VCP default) | ST-Link virtual COM     |
| PD9  | USART3 RX (VCP default) | ST-Link virtual COM     |

## Connecting an LPC55S69-EVK as RoT

The sprot SPI3 pins and GPIO signals are accessible on the ST morpho
connector CN11.  To connect an LPC55S69-EVK board:

| Nucleo (CN11)     | Signal    | LPC55S69-EVK       |
|-------------------|-----------|---------------------|
| Pin 1 (PC10)      | SPI_SCK   | SPI CS/SCK pin      |
| Pin 2 (PC11)      | SPI_MISO  | SPI MISO pin        |
| Pin 3 (PC12)      | SPI_MOSI  | SPI MOSI pin        |
| Pin 17 (PA15)     | SPI_CS    | SPI CS pin          |
| Pin 57 (PD0)      | ROT_IRQ   | GPIO IRQ output     |
| Pin 19/GND        | GND       | GND                 |

