// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

pub use embedded_hal::serial::{Read, Write};
use lpc55_pac as device;
use unwrap_lite::UnwrapLite;

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Error {
    Frame,
    Parity,
    Noise,
    BufFull,
}

pub struct Usart<'a> {
    usart: &'a device::usart0::RegisterBlock,
}

impl<'a> From<&'a device::usart0::RegisterBlock> for Usart<'a> {
    // this function assumes that all necessary configuration of the syscon,
    // flexcomm and gpio have been done
    fn from(usart: &'a device::usart0::RegisterBlock) -> Self {
        usart
            .fifocfg
            .modify(|_, w| w.enabletx().enabled().enablerx().enabled());

        Self { usart }.set_rate(Rate::Baud9600).set_8n1()
    }
}

impl Write<u8> for Usart<'_> {
    type Error = Error;

    fn flush(&mut self) -> nb::Result<(), Error> {
        if self.is_tx_idle() {
            Ok(())
        } else {
            Err(nb::Error::WouldBlock)
        }
    }

    fn write(&mut self, byte: u8) -> nb::Result<(), Error> {
        if !self.is_tx_full() {
            // This is unsafe because we can transmit 7, 8 or 9 bits but the
            // interface can't know what it's been configured for.
            self.usart.fifowr.write(|w| unsafe { w.bits(byte as u32) });
            Ok(())
        } else {
            Err(nb::Error::WouldBlock)
        }
    }
}

impl Read<u8> for Usart<'_> {
    type Error = Error;

    fn read(&mut self) -> nb::Result<u8, Self::Error> {
        if !self.is_rx_empty() {
            let byte = self.usart.fiford.read().rxdata().bits();
            if self.is_rx_frame_err() {
                Err(nb::Error::Other(Error::Frame))
            } else if self.is_rx_parity_err() {
                Err(nb::Error::Other(Error::Parity))
            } else if self.is_rx_noise_err() {
                Err(nb::Error::Other(Error::Noise))
            } else {
                // assume 8 bit data
                Ok(byte.try_into().unwrap_lite())
            }
        } else {
            Err(nb::Error::WouldBlock)
        }
    }
}

pub enum Rate {
    Baud9600,
    Baud19200,
    MBaud1_5,
}

impl<'a> Usart<'a> {
    pub fn set_rate(self, rate: Rate) -> Self {
        // These baud rates assume that the flexcomm0 / uart clock is set to
        // 12Mhz.
        match rate {
            Rate::Baud9600 => {
                self.usart.brg.write(|w| unsafe { w.brgval().bits(124) });
                self.usart.osr.write(|w| unsafe { w.osrval().bits(9) });
            }
            Rate::Baud19200 => {
                self.usart.brg.write(|w| unsafe { w.brgval().bits(124) });
                self.usart.osr.write(|w| unsafe { w.osrval().bits(4) });
            }
            Rate::MBaud1_5 => {
                self.usart.brg.write(|w| unsafe { w.brgval().bits(0) });
                self.usart.osr.write(|w| unsafe { w.osrval().bits(7) });
            }
        }

        self
    }

    pub fn set_8n1(self) -> Self {
        self.usart.cfg.write(|w| unsafe {
            w.paritysel()
                .bits(0)
                .stoplen()
                .bit(false)
                .datalen()
                .bits(1)
                .loop_()
                .normal()
                .syncen()
                .asynchronous_mode()
                .clkpol()
                .falling_edge()
                .enable()
                .enabled()
        });

        self
    }

    pub fn is_tx_full(&self) -> bool {
        !self.usart.fifostat.read().txnotfull().bit()
    }

    pub fn is_rx_empty(&self) -> bool {
        !self.usart.fifostat.read().rxnotempty().bit()
    }

    pub fn is_rx_frame_err(&self) -> bool {
        self.usart.fiford.read().framerr().bit()
    }

    pub fn is_rx_parity_err(&self) -> bool {
        self.usart.fiford.read().parityerr().bit()
    }

    pub fn is_rx_noise_err(&self) -> bool {
        self.usart.fiford.read().rxnoise().bit()
    }

    pub fn is_tx_idle(&self) -> bool {
        self.usart.stat.read().txidle().bit()
    }

    pub fn set_tx_idle_interrupt(&self) {
        self.usart.intenset.modify(|_, w| w.txidleen().set_bit());
    }

    pub fn clear_tx_idle_interrupt(&self) {
        self.usart.intenclr.write(|w| w.txidleclr().set_bit());
    }
}
