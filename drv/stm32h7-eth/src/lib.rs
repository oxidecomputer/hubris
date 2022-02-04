// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hubris driver for the STM32H7 Ethernet MAC and associated stuff.
//!
//! This provides a Hubris-dependent driver framework that is agnostic as to
//! whether it's a separate server, or integrated into another task.
//!
//! It might be useful to have a non-OS-dependent driver core at some point, but
//! we can factor that out after we get this working.

#![no_std]

use core::convert::TryFrom;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

pub mod ring;

use crate::ring::BUFSZ;

/// Control block for ethernet driver.
pub struct Ethernet {
    /// Pointer to the MAC registers.
    mac: &'static device::ethernet_mac::RegisterBlock,
    /// Pointer to the MTL registers.
    _mtl: &'static device::ethernet_mtl::RegisterBlock,
    /// Pointer to the DMA registers.
    dma: &'static device::ethernet_dma::RegisterBlock,
    /// Control of the TX ring.
    tx_ring: crate::ring::TxRing,
    /// Control of the RX ring.
    rx_ring: crate::ring::RxRing,
}

/// As the name implies, this spins until a predicate becomes true, in a crappy
/// way.
///
/// Some of the Ethernet-related events don't have interrupts, leaving us with
/// no choice but to repeatedly reload a status register until the condition
/// becomes true. Doing this naively would starve other tasks of the CPU. So, we
/// sleep between attempts. This winds up sleeping way-the-heck longer than
/// required in most cases, but whaddayagonnado.
fn crappy_spin_until(pred: impl Fn() -> bool) {
    while !pred() {
        userlib::hl::sleep_for(1);
    }
}

impl Ethernet {
    /// Initializes the Ethernet controller, prepares the DMA descriptor rings,
    /// and returns the driver instance.
    ///
    /// # Preconditions
    ///
    /// - Make sure the Ethernet blocks are out of reset.
    /// - Make sure the appropriate pins are configured for your board.
    ///
    /// We might want to fold these operations into `new` in the future, but for
    /// now, you need to do them separately.
    pub fn new(
        mac: &'static device::ethernet_mac::RegisterBlock,
        mtl: &'static device::ethernet_mtl::RegisterBlock,
        dma: &'static device::ethernet_dma::RegisterBlock,
        tx_ring: crate::ring::TxRing,
        rx_ring: crate::ring::RxRing,
    ) -> Self {
        // The DMA register block contains the soft-reset for the entire system.
        // We need to do this soft-reset even straight out of chip reset,
        // because without it, some state is scrambled.
        dma.dmamr.write(|w| w.swr().set_bit());
        // The reset process is autonomous and the swr bit is self clearing, but
        // not instantaneously so. Wait for it to finish. Failing to do this
        // means we're interacting with the registers during reset and is Bad.
        crappy_spin_until(|| !dma.dmamr.read().swr().bit());

        // Okay, we have a freshly reset Ethernet controller.

        // Configure the MDIO clock divider. TODO: this divider is currently
        // hardcoded assuming a ~200MHz AHB frequency.
        const MDIOAR_CR_DIVIDE_BY_102: u8 = 0b0100;
        mac.macmdioar
            .write(|w| unsafe { w.cr().bits(MDIOAR_CR_DIVIDE_BY_102) });
        // Program the DMA bus interface parameters. Early versions of the
        // reference manual contained burst length control bits here, but they
        // appear to have been defeatured in later editions, so we'll just do
        // the defaults.
        dma.dmasbmr.reset();

        // Configure TX burst length to 1, to avoid monopolizing the bus fabric.
        // TODO this is debatable.
        dma.dmactx_cr.write(|w| unsafe { w.txpbl().bits(1) });

        // Configure RX burst length to 1, and also set the size of the receive
        // buffers, which is set centrally rather than on a per-buffer basis.
        //
        // At this point if you set `BUFSZ` to a value that won't fit in a u16,
        // you'll get a runtime panic. TODO: this would make a great static
        // assert....
        dma.dmacrx_cr.write(|w| unsafe {
            w.rxpbl().bits(1).rbsz().bits(u16::try_from(BUFSZ).unwrap())
        });

        // Inform the DMA of the location and length of the TX descriptor ring.
        // Note that we carefully compute the number of descriptors MINUS ONE,
        // which is not mentioned in the reference manual -- the reference
        // manual appears to be wrong.
        let tx_ring_len = u16::try_from(tx_ring.len())
            .map_err(|_| ())
            .unwrap()
            .checked_sub(1)
            .unwrap();
        dma.dmactx_dlar.write(|w| unsafe {
            // The SVD "helpfully" models this field in bits 31:2, so we need to
            // drop our bottom 2 bits.
            w.tdesla().bits(tx_ring.base_ptr() as u32 >> 2)
        });
        dma.dmactx_rlr
            .write(|w| unsafe { w.tdrl().bits(tx_ring_len) });

        // Do the same for the RX ring.
        let rx_ring_len = u16::try_from(rx_ring.len())
            .map_err(|_| ())
            .unwrap()
            .checked_sub(1)
            .unwrap();
        dma.dmacrx_dlar.write(|w| unsafe {
            // The SVD "helpfully" models this field in bits 31:2, so we need to
            // drop our bottom 2 bits.
            w.rdesla().bits(rx_ring.base_ptr() as u32 >> 2)
        });
        dma.dmacrx_rlr
            .write(|w| unsafe { w.rdrl().bits(rx_ring_len) });

        // Poke both tail pointers so the hardware looks at the descriptors. We
        // completely initialize the descriptor array, so the tail pointer is
        // always the end.
        //
        // Doing the same drop-bottom-two-bits stuff that we had to do for DLARs
        // above.
        dma.dmactx_dtpr
            .write(|w| unsafe { w.tdt().bits(tx_ring.tail_ptr() as u32 >> 2) });
        dma.dmacrx_dtpr
            .write(|w| unsafe { w.rdt().bits(rx_ring.tail_ptr() as u32 >> 2) });

        // Central DMA config:

        // We're not appending any additional words to our descriptors.
        dma.dmaccr.write(|w| unsafe { w.dsl().bits(0) });
        // We'd like to hear about successful frame reception.
        dma.dmacier.write(|w| w.nie().set_bit().rie().set_bit());
        // Start transmit and receive DMA.
        dma.dmactx_cr.modify(|_, w| w.st().set_bit());
        dma.dmacrx_cr.modify(|_, w| w.sr().set_bit());

        // Whew!

        // MTL block config:
        // Configure TX queue mode:
        // - Use all 2048 bytes of queue RAM; this is communicated in units of
        //   256 bytes minus 1.
        // - Transmit Store n' Forward so we can do checksum generation
        const QOMR_TQS_8_BLOCKS_OF_256: u8 = 0b111;
        mtl.mtltx_qomr.write(|w| unsafe {
            w.tqs().bits(QOMR_TQS_8_BLOCKS_OF_256).tsf().set_bit()
        });
        // Configure RX queue mode:
        // - Receive Store n' Forward so we can do checksum verification
        mtl.mtlrx_qomr.write(|w| w.rsf().set_bit());

        // MAC block config:
        // Enable promiscuous receive. TODO: we will want to set up the filters
        // later, once we figure out how we assign MAC addresses across the
        // redundant segments.
        mac.macpfr.write(|w| w.pr().set_bit());
        // Force 100mbps full-duplex. TODO: it would be polite to negotiate
        // this, but the KSZ-series switches we talk to won't negotiate.
        mac.maccr.write(|w| {
            w.te()
                .set_bit()
                .re()
                .set_bit()
                .fes()
                .set_bit()
                .dm()
                .set_bit()
                .ipc()
                .set_bit()
        });
        // The peripheral seems to want this to be done in a separate write to
        // MACCR, so:
        // Enable transmit and receive.
        mac.maccr.modify(|_, w| w.te().set_bit().re().set_bit());

        Self {
            mac,
            _mtl: mtl,
            dma,
            tx_ring,
            rx_ring,
        }
    }

    pub fn can_send(&self) -> bool {
        self.tx_ring.is_next_free()
    }

    /// Tries to send a packet, if TX buffer space is available.
    ///
    /// This will attempt to get a free descriptor/buffer from the TX ring. If
    /// successful, it will call `fillout` with the address of the buffer, so
    /// that it can be filled out. `fillout` is expected to overwrite the
    /// (arbitrary) contents of the packet buffer. This routine will then
    /// arrange for the hardware to notice an outgoing packet and return
    /// `Some`.
    ///
    /// If the TX ring is full, this will return `None` without calling
    /// `fillout`.
    pub fn try_send<R>(
        &self,
        len: usize,
        fillout: impl FnOnce(&mut [u8]) -> R,
    ) -> Option<R> {
        let result = self.tx_ring.try_with_next(len, fillout)?;
        // We have enqueued a packet! The hardware may be suspended after
        // discovering no packets to process. Wake it.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        // Poke the tail pointer so the hardware knows to recheck (dropping two
        // bottom bits because svd2rust)
        self.dma.dmactx_dtpr.write(|w| unsafe {
            w.tdt().bits(self.tx_ring.tail_ptr() as u32 >> 2)
        });
        Some(result)
    }

    pub fn can_recv(&self) -> bool {
        self.rx_ring.is_next_free()
    }

    /// Tries to receive a packet, if one is present in the RX ring.
    ///
    /// This will attempt to get a filled-out descriptor/buffer from the RX
    /// ring. If successful, it will call `readout` with the packet's slice.
    /// `readout` can produce a value of some type `R`; after it completes, this
    /// routine will mark the descriptor as empty and return `Some(r)`.
    ///
    /// If there are no packets waiting in the RX ring, this returns `None`
    /// without calling `readout`.
    pub fn try_recv<R>(
        &self,
        readout: impl FnOnce(&mut [u8]) -> R,
    ) -> Option<R> {
        let result = self.rx_ring.try_with_next(readout)?;
        // We have dequeued a packet! The hardware might not realize there is
        // room in the RX queue now. Poke it.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        // Poke the tail pointer so the hardware knows to recheck (dropping two
        // bottom bits because svd2rust)
        self.dma.dmacrx_dtpr.write(|w| unsafe {
            w.rdt().bits(self.rx_ring.tail_ptr() as u32 >> 2)
        });
        Some(result)
    }

    /// Pokes at the controller interrupt status registers to handle and clear
    /// an interrupt condition.
    ///
    /// Returns two flags: whether the interrupt condition indicated that
    /// packets had been successfully transmitted and received, respectively.
    /// (This is not the _best_ API but we can change it once we figure out how
    /// it wants to be used.)
    pub fn on_interrupt(&self) -> (bool, bool) {
        let (mut packet_transmitted, mut packet_received) = (false, false);

        // Diagnosing an ETH IRQ is kind of involved, because the IRQs are all
        // muxed. We'll start out at the DMA interrupt summary register.
        let dmaisr = self.dma.dmaisr.read();
        if dmaisr.dc0is().bit() {
            // The DMA unit has an interrupt (on "channel 0" which is the only
            // channel)
            let dmacsr = self.dma.dmacsr.read();
            if dmacsr.ri().bit() {
                // Received a packet.
                packet_received = true;
                // Clear the interrupt. And the summary bit. Yes, we have to
                // clear the summary bit, it is automatically set but not
                // cleared.
                self.dma.dmacsr.write(|w| w.nis().set_bit().ri().set_bit());
            }
            if dmacsr.ti().bit() {
                // Transmitted a packet.
                packet_transmitted = true;
                // Clear the interrupt. And the summary bit. Yes, we have to
                // clear the summary bit, it is automatically set but not
                // cleared.
                self.dma.dmacsr.write(|w| w.nis().set_bit().ti().set_bit());
            }
        }
        if dmaisr.macis().bit() {
            // The MAC has an interrupt. We do not enable any MAC interrupts.
        }
        if dmaisr.mtlis().bit() {
            // The MTL has an interrupt. We do not enable any MTL interrupts.
        }

        (packet_transmitted, packet_received)
    }

    /// Kicks off a SMI write of `value` to PHY address `phy`, register number
    /// `register`. When this function returns, the write has started. It will
    /// complete asynchronously, so feel free to do other things. Any attempt to
    /// do other SMI operations will synchronize and block until the SMI unit is
    /// free.
    ///
    /// Note that this function does not modify the extended page access
    /// register (31); if you are using the `PhyRw` trait, then the extended
    /// page access register may not be set to 0, so this could return values
    /// from a register on an extended page!
    pub fn smi_write(&mut self, phy: u8, register: impl Into<u8>, value: u16) {
        // Wait until peripheral is free.
        crappy_spin_until(|| !self.is_smi_busy());

        const WRITE: u8 = 0b01;

        // Load data, then load address + start transaction. The address+start
        // must be done using modify because the clock config is in the same
        // register.
        self.mac.macmdiodr.write(|w| unsafe { w.md().bits(value) });
        self.mac.macmdioar.modify(|_, w| unsafe {
            w.pa()
                .bits(phy)
                .rda()
                .bits(register.into())
                .goc()
                .bits(WRITE)
                .mb()
                .set_bit()
        });
    }

    /// Performs a SMI read from PHY address `phy`, register number `register`,
    /// and returns the result.
    ///
    /// Note that this function does not modify the extended page access
    /// register (31); if you are using the `PhyRw` trait, then the extended
    /// page access register may not be set to 0, so this could return values
    /// from a register on an extended page!
    pub fn smi_read(&mut self, phy: u8, register: impl Into<u8>) -> u16 {
        // Wait until peripheral is free.
        crappy_spin_until(|| !self.is_smi_busy());

        // Load address + start transaction
        const READ: u8 = 0b11;
        self.mac.macmdioar.modify(|_, w| unsafe {
            w.pa()
                .bits(phy)
                .rda()
                .bits(register.into())
                .goc()
                .bits(READ)
                .mb()
                .set_bit()
        });

        // Wait until it finishes.
        crappy_spin_until(|| !self.is_smi_busy());

        self.mac.macmdiodr.read().md().bits()
    }

    fn is_smi_busy(&self) -> bool {
        self.mac.macmdioar.read().mb().bit()
    }
}

/// Standard MDIO registers laid out in IEEE 802.3 standard clause 22. Vendors
/// often add to this set in the 16+ range.
///
/// If you are using the VSC7448 / VSC85xx / KSZ8463, then your task includes
/// the `vsc7448-pac` crate.  This crate defines a more complete set of
/// registers, including various extended pages, and has bitfield definitions;
/// you may consider using them instead!
pub enum SmiClause22Register {
    Control = 0,
    Status = 1,
    PhyIdent2 = 2,
    PhyIdent3 = 3,
    AutoNegAdvertisement = 4,
    AutoNegPartnerAbility = 5,
    AutoNegExpansion = 6,
    AutoNegNextPageTransmit = 7,
    AutoNegPartnerReceivedNextPage = 8,
    MasterSlaveControl = 9,
    MasterSlaveStatus = 10,
    PseControl = 11,
    PseStatus = 12,
    MmdAccessControl = 13,
    MmdAccessAddressData = 14,
    ExtendedStatus = 15,
}

impl From<SmiClause22Register> for u8 {
    fn from(x: SmiClause22Register) -> Self {
        x as u8
    }
}

#[cfg(feature = "with-smoltcp")]
pub struct OurRxToken<'a>(&'a Ethernet);

#[cfg(feature = "with-smoltcp")]
impl<'a> smoltcp::phy::RxToken for OurRxToken<'a> {
    fn consume<R, F>(
        self,
        _timestamp: smoltcp::time::Instant,
        f: F,
    ) -> smoltcp::Result<R>
    where
        F: FnOnce(&mut [u8]) -> smoltcp::Result<R>,
    {
        self.0
            .try_recv(f)
            .expect("we checked RX availability earlier")
    }
}

#[cfg(feature = "with-smoltcp")]
pub struct OurTxToken<'a>(&'a Ethernet);

#[cfg(feature = "with-smoltcp")]
impl<'a> smoltcp::phy::TxToken for OurTxToken<'a> {
    fn consume<R, F>(
        self,
        _timestamp: smoltcp::time::Instant,
        len: usize,
        f: F,
    ) -> smoltcp::Result<R>
    where
        F: FnOnce(&mut [u8]) -> smoltcp::Result<R>,
    {
        self.0
            .try_send(len, f)
            .expect("TX token existed without descriptor available")
    }
}

#[cfg(feature = "with-smoltcp")]
impl<'a> smoltcp::phy::Device<'a> for Ethernet {
    type RxToken = OurRxToken<'a>;
    type TxToken = OurTxToken<'a>;

    fn receive(&'a mut self) -> Option<(Self::RxToken, Self::TxToken)> {
        // Note: smoltcp wants a transmit token every time it receives a packet,
        // so if the tx queue fills up, we stop being able to receive. I'm ...
        // not sure why this is desirable, but there we go.
        //
        // Note that the can_recv and can_send checks remain valid because the
        // token mutably borrows the phy.
        if self.can_recv() && self.can_send() {
            Some((OurRxToken(self), OurTxToken(self)))
        } else {
            None
        }
    }

    fn transmit(&'a mut self) -> Option<Self::TxToken> {
        if self.can_send() {
            Some(OurTxToken(self))
        } else {
            None
        }
    }

    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        let mut caps = smoltcp::phy::DeviceCapabilities::default();
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(1514 * self.tx_ring.len());

        use smoltcp::phy::Checksum;
        caps.checksum.ipv4 = Checksum::None;
        caps.checksum.udp = Checksum::None;
        #[cfg(feature = "ipv4")]
        {
            caps.checksum.icmpv4 = Checksum::None;
        }
        #[cfg(feature = "ipv6")]
        {
            caps.checksum.icmpv6 = Checksum::None;
        }
        caps
    }
}
