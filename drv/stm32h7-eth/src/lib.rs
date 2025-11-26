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

use core::sync::atomic::{self, Ordering};

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

    /// Pointer to the timer registers. We use the timer for timing MDIO
    /// transactions, since the MDIO hardware doesn't provide any kind of
    /// interrupts.
    ///
    /// The PAC models all the timers as distinct types, so if you'd like to use
    /// a different timer ... well, sorry.
    mdio_timer: &'static device::tim16::RegisterBlock,
    /// Notification mask for the timer interrupt.
    mdio_timer_irq_mask: u32,
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
        mdio_timer: &'static device::tim16::RegisterBlock,
        mdio_timer_irq_mask: u32,
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

        // Poke the receive tail pointer so that the hardware looks at
        // descriptors. We completely initialize the descriptor array, so the
        // tail pointer is always as close to the end as we can make it.
        //
        // Doing the same drop-bottom-two-bits stuff that we had to do for DLARs
        // above.
        //
        // We don't set the transmit tail pointer until we enqueue a packet,
        // as we don't want the hardware to race against software filling in
        // descriptors.
        atomic::fence(Ordering::Release);
        dma.dmacrx_dtpr.write(|w| unsafe {
            w.rdt().bits(rx_ring.first_tail_ptr() as u32 >> 2)
        });

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

        // Disable potential interrupt sources coming from the MMC (management
        // counters) block. These sources are not otherwise gated, and are on by
        // default until masked. Failing to mask these interrupt sources will
        // start producing interrupts much later, as counters hit their halfway
        // point (generally about 2**31). The interrupt code below is not
        // prepared to handle these counter interrupts.
        mac.mmc_rx_interrupt_mask.write(|w| {
            // Safety: The stm32h7 0.14 crate doesn't model RXLPITRCIM (bit 27),
            // but it's defined in the reference manual and required to disable
            // an interrupt, so, no safety implications.
            unsafe {
                w.bits(1 << 27);
            }

            w.rxcrcerpim().set_bit();
            w.rxalgnerpim().set_bit();
            w.rxucgpim().set_bit();
            w.rxlpiuscim().set_bit();
            w
        });
        mac.mmc_tx_interrupt_mask.write(|w| {
            // Safety: The stm32h7 0.14 crate doesn't model TXLPITRCIM (bit 27)
            // but it's defined in the reference manual and required to disable
            // an interrupt, so, no safety implications.
            unsafe {
                w.bits(1 << 27);
            }

            w.txscolgpim().set_bit();
            w.txmcolgpim().set_bit();
            w.txgpktim().set_bit();
            w.txlpiuscim().set_bit();
            w
        });

        #[cfg(feature = "vlan")]
        {
            // If we're in VLAN mode, we _only_ support VLAN operation.
            // Every incoming packet should be tagged with a VID, and we
            // expect to insert a VID in front of every outgoing packet.
            mac.macvtr.write(|w| unsafe {
                w.evls()
                    .bits(0b11) // Always strip VLAN tag on receive
                    .evlrxs()
                    .set_bit() // Enable VLAN tag in Rx status
            });

            // Configure the Tx path to insert the VLAN tag based on the
            // context descriptor. This is confusing, because different parts
            // of the datasheet disagree whether VLTI should be set or cleared;
            // this is checked experimentally.
            mac.macvir.write(
                |w| w.vlti().set_bit(), // insert tag from context descriptor
            );
        }

        // The peripheral seems to want this to be done in a separate write to
        // MACCR, so:
        // Enable transmit and receive.
        mac.maccr.modify(|_, w| w.te().set_bit().re().set_bit());

        // Configure our timer, but leave it disabled.
        mdio_timer.cr1.write(|w| {
            // Enable one-pulse mode to use the timer as a one-shot.
            w.opm().set_bit();
            w
        });
        mdio_timer.dier.write(|w| {
            // Enable interrupt on update (rollover).
            w.uie().set_bit();
            w
        });
        // Configure the timer's prescaler to use the same factor we chose for
        // MDIO, above. TODO: this may need to be scaled as the reference clocks
        // may not be the same.
        mdio_timer.psc.write(|w| w.psc().bits(102));

        Self {
            mac,
            _mtl: mtl,
            dma,
            tx_ring,
            rx_ring,
            mdio_timer,
            mdio_timer_irq_mask,
        }
    }

    /// Maximum number of packets that can be sent in a burst, assuming the
    /// queue is totally clear.
    pub fn max_tx_burst_len(&self) -> usize {
        self.tx_ring.len()
    }

    // This function is identical in the VLAN and non-VLAN cases, so it lives
    // in the main impl block
    pub fn can_send(&self) -> bool {
        self.tx_ring.is_next_free()
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

    /// Notifies the DMA hardware that space is available in the Rx ring
    fn rx_notify(&self) {
        // We have dequeued a packet! The hardware might not realize there is
        // room in the RX queue now. Poke it.
        atomic::fence(Ordering::Release);
        // Poke the tail pointer so the hardware knows to recheck (dropping two
        // bottom bits because svd2rust).
        self.dma.dmacrx_dtpr.write(|w| unsafe {
            w.rdt().bits(self.rx_ring.next_tail_ptr() as u32 >> 2)
        });
    }

    /// Notifies the DMA hardware that a packet is available in the Tx ring
    fn tx_notify(&self) {
        // We have enqueued a packet! The hardware may be suspended after
        // discovering no packets to process. Wake it.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        // Poke the tail pointer so the hardware knows to recheck (dropping two
        // bottom bits because svd2rust)
        self.dma.dmactx_dtpr.write(|w| unsafe {
            w.tdt().bits(self.tx_ring.next_tail_ptr() as u32 >> 2)
        });
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
    pub fn smi_write(&self, phy: u8, register: impl Into<u8>, value: u16) {
        // Wait until peripheral is free. This spin loop should not spin in
        // practice because we block waiting for any operations we issue.
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
        self.smi_timer_wait();
    }

    /// Performs a SMI read from PHY address `phy`, register number `register`,
    /// and returns the result.
    ///
    /// Note that this function does not modify the extended page access
    /// register (31); if you are using the `PhyRw` trait, then the extended
    /// page access register may not be set to 0, so this could return values
    /// from a register on an extended page!
    pub fn smi_read(&self, phy: u8, register: impl Into<u8>) -> u16 {
        // Wait until peripheral is free. This spin loop should not spin in
        // practice because we block waiting for any operations we issue.
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
        self.smi_timer_wait();
        self.mac.macmdiodr.read().md().bits()
    }

    /// Waits for an MDIO/SMI operation to complete, using dead-reckoning
    fn smi_timer_wait(&self) {
        // An MDIO/SMI operation always consists of
        // - 32 bit preamble
        // - start bit
        // - 2 bit opcode
        // - 5 bit phy address
        // - 5 bit register address
        // - 2 turnaround bits
        // - 16 bit response
        // ...for a total of 63 bits.
        const MDIO_BITS: usize = 63;
        // So we want to program the timer to count up to that many bits, and
        // then interrupt us. Since the ARR value is _included_ in the count,
        // this actually counts out our number of bits _plus one,_ and that's
        // okay because it ensures we've got some padding.
        self.mdio_timer
            .arr
            .write(|w| w.arr().bits(MDIO_BITS as u16));
        self.mdio_timer.cnt.write(|w| w.cnt().bits(0));
        // Force update
        self.mdio_timer.egr.write(|w| w.ug().set_bit());
        // Clear existing interrupt flags.
        self.mdio_timer.sr.write(|w| w.uif().clear_bit());
        // Go!
        self.mdio_timer.cr1.modify(|_, w| w.cen().set_bit());
        // Wait for it. Avoid spurious notifications by checking if the timer
        // has disabled itself before proceeding.
        loop {
            userlib::sys_irq_control(self.mdio_timer_irq_mask, true);
            userlib::sys_recv_notification(self.mdio_timer_irq_mask);
            if !self.mdio_timer.cr1.read().cen().bit() {
                break;
            }
        }

        if self.is_smi_busy() {
            panic!();
        }
    }

    fn is_smi_busy(&self) -> bool {
        self.mac.macmdioar.read().mb().bit()
    }
}

#[cfg(not(feature = "vlan"))]
impl Ethernet {
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
        self.tx_notify();
        Some(result)
    }

    pub fn can_recv(&self) -> bool {
        let (can_recv, any_dropped) = self.rx_ring.is_next_free();
        if any_dropped {
            self.rx_notify();
        }
        can_recv
    }

    /// Receives a packet from the Rx ring, calling `readout` on it and
    /// returning its value.
    ///
    /// This function must only be called when there is a valid (owned,
    /// non-error) descriptor at the front of the ring, as checked by
    /// `can_recv`.  Otherwise, it will panic.
    pub fn recv<R>(&self, readout: impl FnOnce(&mut [u8]) -> R) -> R {
        let result = self.rx_ring.with_next(readout);
        self.rx_notify();
        result
    }
}

#[cfg(feature = "vlan")]
impl Ethernet {
    /// Same as `try_recv`, but only receiving packets that match a particular
    /// VLAN tag. This is only expected to be called from an `RxToken`,
    /// meaning we know that there's already a valid packet in the buffer;
    /// it will panic if this requirement is broken.
    pub fn vlan_recv<R>(
        &self,
        vid: u16,
        readout: impl FnOnce(&mut [u8]) -> R,
    ) -> R {
        let result = self.rx_ring.vlan_with_next(vid, readout);
        self.rx_notify();
        result
    }

    /// Checks whether the next slot on the Rx buffer is owned by userspace
    /// and has a matching VLAN id. Packets without a VID or with a VID
    /// that isn't valid for _any VLAN_ are dropped by the Rx ring during this
    /// function to prevent them from clogging up the system. Packets with
    /// a VID that doesn't match `vid` but is in `vid_range` will not be
    /// dropped, but this function will return `false` in that case.
    pub fn vlan_can_recv(&self, vid: u16, vlans: &[u16]) -> bool {
        let (can_recv, any_dropped) =
            self.rx_ring.vlan_is_next_free(vid, vlans);
        if any_dropped {
            self.rx_notify();
        }
        can_recv
    }

    /// Same as `try_send`, but attaching the given VLAN tag to the outgoing
    /// packet (if present)
    #[cfg(feature = "vlan")]
    pub fn vlan_try_send<R>(
        &self,
        len: usize,
        vid: u16,
        fillout: impl FnOnce(&mut [u8]) -> R,
    ) -> Option<R> {
        let result = self.tx_ring.vlan_try_with_next(len, vid, fillout)?;
        self.tx_notify();
        Some(result)
    }
}
