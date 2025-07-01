use crate::{I2cController, Register, I2cTargetControl};
use drv_stm32xx_sys_api as sys_api;

use ringbuf::*;

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
enum Trace {
    Addr(Register, u32),
    AddrMatch,
    AddrNack(u8),
    RxReg(Register, u32),
    Rx(u8, u8),
    RxNack(u8, u8),
    Tx(u8, u8),
    TxBogus(u8),
    TxOverrun(u8),
    TxReg(Register, u32),
    WaitAddr,
    WaitRx,
    WaitTx,
    Stop,
    RepeatedStart(#[count(children)] bool),
    #[count(skip)]
    None,
}
counted_ringbuf!(Trace, 48, Trace::None);

pub struct Target<'a>(pub I2cController<'a>);

impl Target<'_> {
    pub fn enable(&self, sys: &sys_api::Sys) {
        self.0.enable(sys)
    }

    fn configure_as_target(&self) {
        let i2c = self.0.registers;

        // Disable PE
        i2c.cr1.write(|w| w.pe().clear_bit());

        self.0.configure_timing(i2c);

        #[rustfmt::skip]
        i2c.oar1.modify(|_, w| { w
            .oa1en().clear_bit()                    // own-address disable 
        });

        #[rustfmt::skip]
        i2c.oar2.modify(|_, w| { w
            .oa2en().set_bit()                  // own-address-2 enable
            .oa2msk().bits(0b111)                // mask 7 == match all
        });

        #[rustfmt::skip]
        i2c.cr1.modify(|_, w| { w
            .gcen().clear_bit()           // disable General Call
            .nostretch().clear_bit()      // enable clock stretching
            .sbc().clear_bit()            // disable byte control 
            .errie().clear_bit()          // \
            .tcie().clear_bit()           //  |
            .stopie().clear_bit()         //  | disable
            .nackie().clear_bit()         //  | all
            .addrie().clear_bit()         //  | interrupt
            .rxie().clear_bit()           //  | sources
            .txie().clear_bit()           // /
        });

        i2c.cr1.modify(|_, w| w.pe().set_bit());
    }

    pub fn operate_as_target(
        &self,
        ctrl: &I2cTargetControl,
        mut initiate: impl FnMut(u8) -> bool,
        mut rxbyte: impl FnMut(u8, u8),
        mut txbyte: impl FnMut(u8) -> Option<u8>,
    ) -> ! {
        // Note: configure_as_target toggles the CR1.PE bit, which has the side
        // effect of clearing all flags.
        self.configure_as_target();

        let i2c = self.0.registers;
        let notification = self.0.notification;

        'addrloop: loop {
            // Flush our TXDR. TODO: does this ever matter in practice? Are we
            // making it to this point with TXE clear?
            i2c.isr.modify(|_, w| w.txe().set_bit());

            // Wait to be addressed.
            let (is_write, addr) = loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::Addr(Register::ISR, isr.bits()));

                // We expect STOPF to have been handled by the transaction loop
                // below, but given that there may be other irrelevant
                // transactions on the bus, we'll go ahead and clear it here.
                if isr.stopf().is_stop() {
                    i2c.icr.write(|w| w.stopcf().set_bit());
                    continue;
                }

                // ADDR being set means that we've been addressed -- either as a
                // result of a START condition, or a repeated START punted by
                // the transaction loop below.
                if isr.addr().is_match() {
                    i2c.icr.write(|w| w.addrcf().set_bit());
                    ringbuf_entry!(Trace::AddrMatch);
                    break (isr.dir().is_write(), isr.addcode().bits());
                }

                // Enable the interrupt sources we care about. Note that despite
                // handling STOPF above, we don't enable an interrupt on it,
                // because we don't actually care.
                i2c.cr1.modify(|_, w| w.addrie().set_bit());
                ringbuf_entry!(Trace::WaitAddr);
                (ctrl.enable)(notification);
                (ctrl.wfi)(notification);
                // Turn interrupt sources back off.
                i2c.cr1.modify(|_, w| w.addrie().clear_bit());
            };

            // See if we want to initiate with this address, NACK'ing it if
            // not.  Note that if we are being sent bytes, it is too late to
            // NACK the address itself; the NACK will be on the write.
            //
            // Note also that, if we decline to respond to the address, we're
            // still going to go through all the transaction machinery below!
            // This helps to ensure that we maintain the flags correctly. It has
            // the semi-strange side effect that we will process transactions
            // sent to any other device on the bus, and send responses that keep
            // SDA in its recessive (high) state so the other device can talk.
            //
            // This means we will inject our clock stretching intervals into
            // _all traffic_ and is probably worth fixing (TODO).
            let initiated = initiate(addr);

            if !initiated {
                // NACK the first byte.
                i2c.cr2.modify(|_, w| w.nack().set_bit());
                ringbuf_entry!(Trace::AddrNack(addr));
            }

            if is_write {
                // During the write phase, the host sends bytes our way, and we
                // have the opportunity to ACK/NACK each one. This phase
                // continues until the host generates either a repeated start or
                // a stop condition.
                //
                // If we're not responding to this transaction, we have set the
                // NACK flag above. However, this only applies to one byte. The
                // host is free to continue clocking us after a NACK, which we
                // handle below.
                'rxloop: loop {
                    let isr = i2c.isr.read();
                    ringbuf_entry!(Trace::RxReg(Register::ISR, isr.bits()));

                    // Note: the order of interrupt flag handling in this
                    // routine is important. More details interleaved below.

                    // Check for and handle RXNE first, to ensure that incoming
                    // data gets handled and isn't left around waiting for
                    // later. We can be confident that the data waiting in RX is
                    // from this transaction, and not a later transaction on the
                    // far side of a STOP/NACK, because we have configured the
                    // controller to clock-stretch if we're repeatedly
                    // addressed, preventing the reception of further data until
                    // we get out of this loop and do it all over again.
                    if isr.rxne().is_not_empty() {
                        // Always take the byte from the shift register, even if
                        // we're ignoring it, lest the shift register clog up.
                        let rx = i2c.rxdr.read().rxdata().bits();

                        if initiated {
                            ringbuf_entry!(Trace::Rx(addr, rx));
                            rxbyte(addr, rx);
                        } else {
                            // We're ignoring this byte. It has already been
                            // NACK'd, and the NACK flag is self-clearing. Ask
                            // to NACK the next. Our request will be canceled by
                            // STOP or ADDR.
                            i2c.cr2.modify(|_, w| w.nack().set_bit());
                            ringbuf_entry!(Trace::RxNack(addr, rx));
                        }

                        // Bounce up to the top of the loop, which will cause
                        // other flags to get handled.
                        continue 'rxloop;
                    }

                    // If we have seen a STOP condition, our current transaction
                    // is over, and we want to ignore the ADDR flag being set
                    // since that'll get handled at the top of the loop.
                    if isr.stopf().is_stop() {
                        ringbuf_entry!(Trace::Stop);
                        i2c.icr.write(|w| w.stopcf().set_bit());
                        continue 'addrloop;
                    }

                    // Note: during this phase we are receiving data from the
                    // controller and generating ACKs/NACKs. This means the
                    // NACKF is irrelevant, as it's only set when a NACK is
                    // _received._

                    // If we've processed all incoming data and have not seen a
                    // STOP condition, then the ADDR flag being set means we've
                    // been addressed in a repeated start.
                    if isr.addr().is_match() {
                        i2c.icr.write(|w| w.addrcf().set_bit());

                        //
                        // If we have an address match, check to see if this is
                        // change in direction; if it is, break out of our receive
                        // loop.
                        //
                        if !isr.dir().is_write() {
                            ringbuf_entry!(Trace::RepeatedStart(true));
                            break 'rxloop;
                        }

                        // Repeated start without a direction change is
                        // slightly weird, but, we'll handle it as best we can.
                        ringbuf_entry!(Trace::RepeatedStart(false));
                        continue 'rxloop;
                    }

                    // Enable the interrupt sources we use.
                    #[rustfmt::skip]
                    i2c.cr1.modify(|_, w| {
                        w.stopie().set_bit()
                            .addrie().set_bit()
                            .rxie().set_bit()
                    });

                    ringbuf_entry!(Trace::WaitRx);
                    (ctrl.enable)(notification);
                    (ctrl.wfi)(notification);

                    // Turn them back off before we potentially break out of the
                    // loop above.
                    #[rustfmt::skip]
                    i2c.cr1.modify(|_, w| {
                        w.stopie().clear_bit()
                            .addrie().clear_bit()
                            .rxie().clear_bit()
                    });
                }
            }

            'txloop: loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::TxReg(Register::ISR, isr.bits()));

                // First, we want to see if we're still transmitting.

                // See if the host has NACK'd us. When our peripheral receives a
                // NACK, it releases the SDA/SCL lines and stops setting TXIS.
                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| w.nackcf().set_bit());
                    // Do _not_ abort the transmission at this point. The host
                    // may do something dumb like continue reading past our
                    // NACK. Wait for STOP or ADDR (repeated start).

                    // Fall through to the other flag handling below.
                }

                // A STOP condition _always_ indicates that the transmission is
                // over... even if we don't think we're done sending. So,
                // process it before attempting to put more data on the wire in
                // response to TXIS below.
                if isr.stopf().is_stop() {
                    i2c.icr.write(|w| w.stopcf().set_bit());
                    break 'txloop;
                }

                // ADDR will be set by a repeated start. We'll handle it by
                // _leaving it set_ and bopping back up to the top to start a
                // new transaction.
                if isr.addr().is_match() {
                    continue 'addrloop;
                }

                // If we get here, it means the host is still clocking bytes out
                // of us, so we need to send _something_ or we'll lock the bus
                // forever.
                if isr.txis().is_empty() {
                    // This byte is deliberately indistinguishable from no
                    // activity from the target on the bus. This is
                    // important since we're wired-ANDing our output with
                    // any other I2C devices at this point.
                    const FILLER: u8 = 0xff;

                    if initiated {
                        match txbyte(addr) {
                            Some(byte) => {
                                ringbuf_entry!(Trace::Tx(addr, byte));
                                i2c.txdr.write(|w| w.txdata().bits(byte));
                            }
                            None => {
                                //
                                // The initiator is asking for more than we've
                                // got, either because it is reading from an
                                // invalid device address, or it wrote to an
                                // invalid register/address, or it's asking
                                // for more data than is supported.  However
                                // it's happening, we don't have a way of
                                // NACK'ing the request once our address is
                                // ACK'd, so we will just return filler data
                                // until the iniatior releases us from their
                                // grip.
                                //
                                ringbuf_entry!(Trace::TxOverrun(addr));
                                i2c.txdr.write(|w| w.txdata().bits(FILLER));
                            }
                        }
                    } else {
                        ringbuf_entry!(Trace::TxBogus(addr));
                        i2c.txdr.write(|w| w.txdata().bits(FILLER));
                    }

                    // Don't WFI because there may be more work to do
                    // immediately.
                    continue 'txloop;
                }

                // Enable the interrupt sources we care about.
                #[rustfmt::skip]
                i2c.cr1.modify(|_, w| {
                    w.txie().set_bit()
                        .addrie().set_bit()
                        .nackie().set_bit()
                        .stopie().set_bit()
                });
                ringbuf_entry!(Trace::WaitTx);
                (ctrl.enable)(notification);
                (ctrl.wfi)(notification);
                // Turn interrupt sources back off.
                #[rustfmt::skip]
                i2c.cr1.modify(|_, w| {
                    w.txie().clear_bit()
                        .addrie().clear_bit()
                        .nackie().clear_bit()
                        .stopie().clear_bit()
                });
            }
        }
    }
}


